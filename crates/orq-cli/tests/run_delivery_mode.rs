//! A run records what it does when it finishes, its delivery: `--delivery review|push|merge-local`,
//! else the delivery its project saved in this machine's orq.db, else review. It is read once, when
//! the run starts, so a later change to the project's settings never alters a run in flight, and a
//! resumed run keeps the delivery it started with.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::project_settings::{Delivery, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::RunStore;
use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    remote: PathBuf,
    home: PathBuf,
    ssh: PathBuf,
    path: std::ffi::OsString,
    provider: FakeProvider,
    _agent: FakeAgent,
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn world(tag: &str) -> World {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-delivery-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    let ssh = home.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    let provider = FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("claude"), &format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\nexec '{}' \"$@\"\n", agent.path().display()));
    let mut dirs = vec![bin, provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, remote, home, ssh, path, provider, _agent: agent }
}

fn orq(w: &World, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", w.home.join("config"))
        .env("XDG_DATA_HOME", w.home.join("data"))
        .env("XDG_STATE_HOME", w.home.join("state"))
        .env("XDG_CACHE_HOME", w.home.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn save_delivery(w: &World, delivery: Delivery) {
    let base = w.home.clone();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
        "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
        "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
        _ => None,
    })
    .unwrap();
    roots.create_all().unwrap();
    let mut store = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(w.repo.root()).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    let settings = ProjectSettings { delivery, ..ProjectSettings::default() };
    assert!(matches!(store.save_project_settings(&root, revision, &settings).unwrap(), SaveOutcome::Saved { .. }));
}

fn run(w: &World, extra: &[&str]) -> Output {
    let repo = w.repo.root().to_str().unwrap().to_string();
    let mut args = vec!["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn store(w: &World) -> RunStore {
    RunStore::new(&w.home.join("state/orq"))
}

fn only_run_delivery(w: &World, extra: &[&str]) -> Delivery {
    let out = run(w, extra);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let ids = store(w).run_ids().unwrap();
    assert_eq!(ids.len(), 1);
    store(w).load(&ids[0]).unwrap().delivery
}

#[test]
fn a_run_records_the_flag_s_delivery_else_the_project_s_else_review() {
    assert_eq!(only_run_delivery(&world("default"), &[]), Delivery::Review, "no record: review");

    let saved = world("saved");
    save_delivery(&saved, Delivery::Push);
    assert_eq!(only_run_delivery(&saved, &[]), Delivery::Push, "the project's delivery");

    let flagged = world("flagged");
    save_delivery(&flagged, Delivery::Push);
    assert_eq!(only_run_delivery(&flagged, &["--delivery", "merge-local"]), Delivery::MergeLocal, "the flag wins");
}

#[test]
fn a_delivery_flag_that_names_no_mode_is_refused_before_anything_starts() {
    let w = world("refused");
    for bad in ["ship", "merge_local", ""] {
        let out = run(&w, &["--delivery", bad, "--check"]);
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {}", text(&out.stderr));
        assert!(text(&out.stderr).contains("--delivery"), "{bad:?}: {}", text(&out.stderr));
    }
    assert!(store(&w).run_ids().unwrap_or_default().is_empty(), "nothing was recorded");
}

/// A gate that asks the run it belongs to to pause at its next boundary.
fn pausing_gate(w: &World) {
    let script = w.home.join("gate.sh");
    let state = w.home.join("state");
    executable(
        &script,
        &format!(
            "#!/bin/sh\n[ -e '{done}' ] && exit 0\ntouch '{done}'\nXDG_STATE_HOME='{state}' '{orq}' run pause \"$(ls '{state}/orq/runs')\" >/dev/null 2>&1\nexit 0\n",
            done = w.home.join("paused-once").display(),
            orq = env!("CARGO_BIN_EXE_orq"),
            state = state.display(),
        ),
    );
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"{}\"]\nexplain_failures = false\n", script.display())).unwrap();
}

#[test]
fn a_resumed_run_keeps_the_delivery_it_started_with() {
    let w = world("resume");
    pausing_gate(&w);
    save_delivery(&w, Delivery::Push);
    let first = run(&w, &[]);
    assert!(text(&first.stdout).contains("state: paused"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let paused = store(&w).run_ids().unwrap().remove(0);

    // The project's setting changes while the run is paused.
    let base = w.home.clone();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        _ => Some(base.join("other").display().to_string()),
    })
    .unwrap();
    let mut db = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(w.repo.root()).unwrap();
    let revision = db.project_settings(&root).unwrap().revision;
    db.save_project_settings(&root, revision, &ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() }).unwrap();

    let repo = w.repo.root().to_str().unwrap().to_string();
    let resumed = orq(&w, &["run", "resume", &paused, "--yes", "--no-ci-check", "--repo", &repo]);
    assert_eq!(resumed.status.code(), Some(0), "{}\n{}", text(&resumed.stdout), text(&resumed.stderr));
    let new_id = store(&w).run_ids().unwrap().into_iter().find(|id| *id != paused).expect("a resumed run");
    assert_eq!(store(&w).load(&new_id).unwrap().delivery, Delivery::Push, "kept from the paused run");
}

fn ending(w: &World, run_id: &str) -> Option<String> {
    store(w).events(run_id).unwrap().iter().rev().find(|e| e.kind == "run.completed").and_then(|e| e.payload["ending"].as_str().map(str::to_string))
}

fn remote_branch(w: &World, branch: &str) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(&w.remote).args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).output().unwrap();
    out.status.success().then(|| text(&out.stdout).trim().to_string())
}

#[test]
fn a_push_only_run_pushes_its_branch_and_stops_without_opening_a_review() {
    let w = world("push");
    save_delivery(&w, Delivery::Push);
    let out = run(&w, &[]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: pushed"), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("review:"), "no review was opened: {}", text(&out.stdout));

    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert!(remote_branch(&w, &record.branch).is_some(), "the branch {} was pushed", record.branch);
    assert!(!record.worktree.exists(), "a delivered run's worktree is cleaned up: {}", record.worktree.display());
    let opened: Vec<_> = w.provider.calls().into_iter().filter(|c| c.args.first().map(String::as_str) == Some("pr") && c.args.get(1).map(String::as_str) == Some("create")).collect();
    assert!(opened.is_empty(), "no PR/MR was opened: {opened:?}");
    assert_eq!(ending(&w, &id).as_deref(), Some("pushed"));
    let summary = orq_orchestrator::summarize_run(&store(&w).events(&id).unwrap());
    assert_eq!(summary.phase.as_str(), "completed", "a pushed run has ended");
}

#[test]
fn a_push_only_run_still_leaves_its_local_review_report() {
    let w = world("push-report");
    let out = run(&w, &["--delivery", "push"]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);

    let shown = orq(&w, &["review", "show", &id, "--json"]);
    assert!(shown.status.success(), "the Reviews tab has the pushed head's report: {}", text(&shown.stderr));
}

fn rev(dir: &Path, rev: &str) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--verify", "--quiet", rev]).output().unwrap();
    text(&out.stdout).trim().to_string()
}

#[test]
fn a_merge_local_run_fast_forwards_the_checked_out_base_branch_and_pushes_nothing() {
    let w = world("merge");
    let remote_main = rev(&w.remote, "refs/heads/main");
    let out = run(&w, &["--delivery", "merge-local"]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));

    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    let head = rev(w.repo.root(), &format!("refs/heads/{}", record.branch));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), head, "main was fast-forwarded to the run's branch");
    assert_eq!(rev(w.repo.root(), "HEAD"), head, "the checked-out main moved with it");
    assert!(w.repo.root().join("feature.txt").exists(), "the checkout has the run's work");
    assert!(remote_branch(&w, &record.branch).is_none(), "nothing was pushed");
    assert_eq!(rev(&w.remote, "refs/heads/main"), remote_main, "the remote's main is untouched");
    assert!(w.provider.calls().is_empty(), "no PR/MR was looked for or opened: {:?}", w.provider.calls());
    assert_eq!(ending(&w, &id).as_deref(), Some("merged"));
    assert_eq!(orq_orchestrator::summarize_run(&store(&w).events(&id).unwrap()).phase.as_str(), "completed");
    assert!(!record.worktree.exists(), "a merged run's worktree is cleaned up");
}

#[test]
fn a_merge_local_run_into_a_checkout_with_uncommitted_changes_is_blocked_and_keeps_its_work() {
    let w = world("merge-dirty");
    let main_before = rev(w.repo.root(), "refs/heads/main");
    std::fs::write(w.repo.root().join("README.md"), "edited, not committed\n").unwrap();
    let out = run(&w, &["--delivery", "merge-local"]);
    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("uncommitted changes"), "the reason is given: {}", text(&out.stderr));

    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), main_before, "main did not move");
    assert_eq!(std::fs::read_to_string(w.repo.root().join("README.md")).unwrap(), "edited, not committed\n", "the edit is left alone");
    assert!(!rev(w.repo.root(), &format!("refs/heads/{}", record.branch)).is_empty(), "the run's branch is kept");
    assert!(record.worktree.exists(), "the run's worktree is kept");
    assert!(remote_branch(&w, &record.branch).is_none(), "nothing was pushed");
}

/// Commits `file` on the repository's checked-out main, without pushing it.
fn commit_on_local_main(w: &World, file: &str, contents: &str) -> String {
    std::fs::write(w.repo.root().join(file), contents).unwrap();
    git(w.repo.root(), &["add", file]);
    git(w.repo.root(), &["-c", "user.name=Someone", "-c", "user.email=someone@example.invalid", "commit", "--quiet", "-m", &format!("local: {file}")]);
    rev(w.repo.root(), "refs/heads/main")
}

/// A gate that runs `script` (sh) and leaves the `GATE_BASE` of each of its runs as a line in the returned file.
fn gate_that_records_its_base(w: &World, tag: &str, script: &str) -> PathBuf {
    let seen = orq_test_support::process_temp_dir().join(format!("orq-merge-gate-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_file(&seen);
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    let body = format!("echo \"$GATE_BASE\" >> '{}'; {script}", seen.display());
    std::fs::write(w.repo.root().join(".orq/run.toml"), format!("gate = [\"sh\", \"-c\", {}]\n", serde_json::to_string(&body).unwrap())).unwrap();
    seen
}

fn gate_bases(seen: &Path) -> Vec<String> {
    std::fs::read_to_string(seen).unwrap_or_default().lines().map(str::to_string).collect()
}

#[test]
fn a_merge_local_run_whose_base_moved_is_rebased_onto_it_checked_again_and_then_fast_forwarded() {
    let w = world("merge-moved");
    let remote_main = rev(&w.remote, "refs/heads/main");
    // A commit on the local main that the remote does not have: the run rebases onto the remote's
    // main, so local main is no longer behind the run's branch.
    let local_main = commit_on_local_main(&w, "local.txt", "local\n");
    let seen = gate_that_records_its_base(&w, "moved", "true");

    let out = run(&w, &["--delivery", "merge-local"]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    let head = rev(w.repo.root(), &format!("refs/heads/{}", record.branch));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), head, "main was fast-forwarded to the run's branch");
    assert_eq!(rev(w.repo.root(), &format!("{head}~1")), local_main, "the run's work sits on the local main it found");
    assert!(w.repo.root().join("local.txt").exists() && w.repo.root().join("feature.txt").exists(), "the checkout has both");
    assert_eq!(gate_bases(&seen), vec![remote_main.clone(), local_main], "the gate ran again on the rebased branch, told its new base");
    assert_eq!(rev(&w.remote, "refs/heads/main"), remote_main, "nothing was pushed");
    assert_eq!(ending(&w, &id).as_deref(), Some("merged"));
}

#[test]
fn a_merge_local_run_whose_rebase_onto_the_moved_base_conflicts_is_blocked_with_main_left_alone() {
    let w = world("merge-moved-conflict");
    // The run adds feature.txt too, with other contents: the rebase onto local main conflicts.
    let local_main = commit_on_local_main(&w, "feature.txt", "theirs\n");
    let seen = gate_that_records_its_base(&w, "moved-conflict", "true");

    let out = run(&w, &["--delivery", "merge-local"]);

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("stopped on a conflict"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert_eq!(gate_bases(&seen).len(), 1, "the gate is not run on a branch mid-rebase");
    assert!(record.worktree.exists(), "the run's worktree is kept");
}

#[test]
fn a_merge_local_run_whose_base_keeps_moving_is_blocked_after_three_catch_ups() {
    let w = world("merge-moving");
    commit_on_local_main(&w, "local.txt", "local\n");
    // Every gate run moves the local main on again, so the run never catches up.
    let root = w.repo.root().display().to_string();
    let seen = gate_that_records_its_base(
        &w,
        "moving",
        &format!("git -C '{root}' -c user.name=Someone -c user.email=someone@example.invalid commit --quiet --allow-empty -m again"),
    );

    let out = run(&w, &["--delivery", "merge-local"]);

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("main moved since this run rebased onto it"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(gate_bases(&seen).len(), 4, "the first gate and one for each of three catch-ups");
    assert_ne!(rev(w.repo.root(), "refs/heads/main"), rev(w.repo.root(), &format!("refs/heads/{}", record.branch)), "main was not fast-forwarded");
}

#[test]
fn a_merge_local_run_with_rebasing_off_is_blocked_when_its_base_moved_without_catching_up() {
    let w = world("merge-moved-no-rebase");
    let local_main = commit_on_local_main(&w, "local.txt", "local\n");
    let seen = gate_that_records_its_base(&w, "moved-no-rebase", "true");

    let out = run(&w, &["--delivery", "merge-local", "--no-rebase"]);

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("main moved since this run rebased onto it"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert_eq!(gate_bases(&seen).len(), 1, "the run was not rebased and checked again");
}

#[test]
fn a_merge_local_run_whose_gate_fails_after_catching_up_fails_with_main_left_alone() {
    let w = world("merge-moved-red");
    let local_main = commit_on_local_main(&w, "local.txt", "local\n");
    // Green on the branch the run started from, red once local.txt is in it.
    let seen = gate_that_records_its_base(&w, "moved-red", "test ! -e local.txt");

    let out = run(&w, &["--delivery", "merge-local"]);

    assert_eq!(out.status.code(), Some(31), "failed: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("the gate failed"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    assert_eq!(ending(&w, &id).as_deref(), Some("failed"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert!(gate_bases(&seen).len() >= 2, "the gate ran again after the catch-up");
}
