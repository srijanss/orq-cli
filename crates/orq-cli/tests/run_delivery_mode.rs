//! A run records what it does when it finishes, its delivery: `--delivery review|merge-local`,
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

/// The project's record names the removed push delivery, as one saved before it was removed does.
fn save_removed_push(w: &World) {
    save_delivery(w, Delivery::Review);
    let db = rusqlite::Connection::open(w.home.join("data/orq/orq.db")).unwrap();
    assert_eq!(db.execute("UPDATE project_settings SET delivery = 'push'", []).unwrap(), 1);
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
    save_delivery(&saved, Delivery::MergeLocal);
    assert_eq!(only_run_delivery(&saved, &[]), Delivery::MergeLocal, "the project's delivery");

    let flagged = world("flagged");
    save_delivery(&flagged, Delivery::MergeLocal);
    assert_eq!(only_run_delivery(&flagged, &["--delivery", "review"]), Delivery::Review, "the flag wins");
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

const PUSH_REMOVED: &str = "the push delivery was removed: use review (pushes and opens a review) or merge-local";

#[test]
fn the_removed_push_delivery_flag_is_refused_with_what_to_use_instead() {
    let w = world("push-flag");
    for extra in [&["--delivery", "push", "--check"][..], &["--delivery", "push"][..]] {
        let out = run(&w, extra);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {}", text(&out.stderr));
        assert!(text(&out.stderr).contains(PUSH_REMOVED), "{extra:?}: {}", text(&out.stderr));
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
    save_delivery(&w, Delivery::Review);
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
    assert_eq!(store(&w).load(&new_id).unwrap().delivery, Delivery::Review, "kept from the paused run");
}

/// Rewrites a run's record to name the removed push delivery, as one started before it was removed does.
fn record_removed_push(w: &World, run_id: &str) {
    let path = w.home.join("state/orq/runs").join(run_id).join("run.json");
    let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["delivery"] = serde_json::Value::from("push");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn a_run_recorded_with_the_removed_push_delivery_still_lists_but_is_not_resumed_with_it() {
    let w = world("push-record");
    pausing_gate(&w);
    let first = run(&w, &[]);
    assert!(text(&first.stdout).contains("state: paused"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let paused = store(&w).run_ids().unwrap().remove(0);
    record_removed_push(&w, &paused);
    assert_eq!(store(&w).load(&paused).unwrap().delivery, Delivery::Push, "the record still reads");

    let repo = w.repo.root().to_str().unwrap().to_string();
    let listed = orq(&w, &["runs", "--repo", &repo]);
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr));
    assert!(text(&listed.stdout).contains(&paused), "{}", text(&listed.stdout));

    let resumed = orq(&w, &["run", "resume", &paused, "--yes", "--no-ci-check", "--repo", &repo]);
    assert_ne!(resumed.status.code(), Some(0), "{}", text(&resumed.stdout));
    assert!(text(&resumed.stderr).contains(PUSH_REMOVED), "{}", text(&resumed.stderr));
    assert_eq!(store(&w).run_ids().unwrap(), vec![paused.clone()], "no run was started");
}

/// A paused run recorded with the removed push delivery, refused once on resume.
fn paused_push_run(tag: &str) -> (World, String) {
    let w = world(tag);
    pausing_gate(&w);
    let first = run(&w, &[]);
    assert!(text(&first.stdout).contains("state: paused"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let paused = store(&w).run_ids().unwrap().remove(0);
    record_removed_push(&w, &paused);
    (w, paused)
}

fn resumed_with(w: &World, paused: &str, delivery: &str) -> orq_orchestrator::RunRecord {
    let out = run(w, &["--resume", "--delivery", delivery]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let new_id = store(w).run_ids().unwrap().into_iter().find(|id| id != paused).expect("a resumed run");
    store(w).load(&new_id).unwrap()
}

#[test]
fn a_paused_push_run_resumed_with_merge_local_named_merges_locally_and_pushes_nothing() {
    let (w, paused) = paused_push_run("push-resume-merge");
    let remote_main = rev(&w.remote, "refs/heads/main");
    let record = resumed_with(&w, &paused, "merge-local");
    assert_eq!(record.delivery, Delivery::MergeLocal, "the named delivery replaces push");
    assert_eq!(ending(&w, &record.run_id).as_deref(), Some("merged"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), rev(w.repo.root(), &format!("refs/heads/{}", record.branch)));
    assert!(remote_branch(&w, &record.branch).is_none(), "nothing was pushed");
    assert_eq!(rev(&w.remote, "refs/heads/main"), remote_main, "the remote's main is untouched");
    assert!(w.provider.calls().is_empty(), "no review was opened: {:?}", w.provider.calls());
}

#[test]
fn a_paused_push_run_resumed_with_review_named_pushes_and_opens_a_review() {
    let (w, paused) = paused_push_run("push-resume-review");
    let record = resumed_with(&w, &paused, "review");
    assert_eq!(record.delivery, Delivery::Review, "the named delivery replaces push");
    assert!(remote_branch(&w, &record.branch).is_some(), "the branch was pushed");
    assert!(!w.provider.calls().is_empty(), "a review was looked for or opened");
    assert_ne!(ending(&w, &record.run_id).as_deref(), Some("merged"));
}

#[test]
fn a_paused_run_resumes_with_its_own_delivery_when_its_project_now_names_the_removed_push() {
    let w = world("resume-push-saved");
    pausing_gate(&w);
    let first = run(&w, &[]);
    assert!(text(&first.stdout).contains("state: paused"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let paused = store(&w).run_ids().unwrap().remove(0);
    save_removed_push(&w);

    let repo = w.repo.root().to_str().unwrap().to_string();
    let resumed = orq(&w, &["run", "resume", &paused, "--yes", "--no-ci-check", "--repo", &repo]);
    assert_eq!(resumed.status.code(), Some(0), "{}\n{}", text(&resumed.stdout), text(&resumed.stderr));
    let new_id = store(&w).run_ids().unwrap().into_iter().find(|id| *id != paused).expect("a resumed run");
    assert_eq!(store(&w).load(&new_id).unwrap().delivery, Delivery::Review, "kept from the paused run");
}

fn ending(w: &World, run_id: &str) -> Option<String> {
    store(w).events(run_id).unwrap().iter().rev().find(|e| e.kind == "run.completed").and_then(|e| e.payload["ending"].as_str().map(str::to_string))
}

fn remote_branch(w: &World, branch: &str) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(&w.remote).args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).output().unwrap();
    out.status.success().then(|| text(&out.stdout).trim().to_string())
}

#[test]
fn a_project_whose_saved_delivery_is_the_removed_push_refuses_to_start_a_run_until_another_is_chosen() {
    let w = world("push-saved");
    save_removed_push(&w);
    for extra in [&["--check"][..], &[][..]] {
        let out = run(&w, extra);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {}\n{}", text(&out.stdout), text(&out.stderr));
        assert!(text(&out.stderr).contains(PUSH_REMOVED), "{extra:?}: {}", text(&out.stderr));
    }
    assert!(store(&w).run_ids().unwrap_or_default().is_empty(), "nothing was recorded");
    assert!(w.provider.calls().is_empty(), "nothing was asked of the provider");

    // Naming a delivery on the command line is a choice, so the run starts with it.
    assert_eq!(only_run_delivery(&w, &["--delivery", "merge-local"]), Delivery::MergeLocal);
}

#[test]
fn a_merge_local_run_still_leaves_its_local_review_report() {
    let w = world("merge-report");
    let out = run(&w, &["--delivery", "merge-local"]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);

    let shown = orq(&w, &["review", "show", &id, "--json"]);
    assert!(shown.status.success(), "the Reviews tab has the merged head's report: {}", text(&shown.stderr));
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
    // The run adds feature.txt; the checkout has its own, staged but not committed.
    std::fs::write(w.repo.root().join("feature.txt"), "edited, not committed\n").unwrap();
    git(w.repo.root(), &["add", "feature.txt"]);
    let out = run(&w, &["--delivery", "merge-local"]);
    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("uncommitted changes"), "the reason is given: {}", text(&out.stderr));

    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), main_before, "main did not move");
    assert_eq!(std::fs::read_to_string(w.repo.root().join("feature.txt")).unwrap(), "edited, not committed\n", "the edit is left alone");
    assert!(!rev(w.repo.root(), &format!("refs/heads/{}", record.branch)).is_empty(), "the run's branch is kept");
    assert!(record.worktree.exists(), "the run's worktree is kept");
    assert!(remote_branch(&w, &record.branch).is_none(), "nothing was pushed");
}

/// A merge-local run blocked because the checkout has its own feature.txt staged, which the run adds.
fn blocked_on_a_dirty_checkout(tag: &str) -> (World, String) {
    let w = world(tag);
    std::fs::write(w.repo.root().join("feature.txt"), "edited, not committed\n").unwrap();
    git(w.repo.root(), &["add", "feature.txt"]);
    let out = run(&w, &["--delivery", "merge-local"]);
    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    (w, id)
}

#[test]
fn merging_a_run_blocked_on_a_dirty_checkout_with_set_aside_delivers_it_and_names_the_stash_its_conflicting_changes_are_in() {
    let (w, blocked) = blocked_on_a_dirty_checkout("merge-set-aside");
    let repo = w.repo.root().to_str().unwrap().to_string();
    let out = orq(&w, &["run", "merge", &blocked, "--set-aside", "--no-ci-check", "--repo", &repo]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));

    let merged = store(&w).run_ids().unwrap().into_iter().find(|id| *id != blocked).expect("a resumed run");
    let record = store(&w).load(&merged).unwrap();
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), rev(w.repo.root(), &format!("refs/heads/{}", record.branch)), "main was fast-forwarded");
    assert_eq!(ending(&w, &merged).as_deref(), Some("merged"));
    let stash = rev(w.repo.root(), "refs/stash");
    assert!(!stash.is_empty(), "the conflicting changes are kept in a stash entry");
    let completed = store(&w).events(&merged).unwrap().into_iter().rev().find(|e| e.kind == "run.completed").unwrap();
    let note = completed.payload["reason"].as_str().unwrap_or_default().to_string();
    assert!(note.contains(&format!("your changes to feature.txt conflicted with this run and are in stash {stash} (orq set-aside {merged})")), "{note}");
    let file = std::fs::read_to_string(w.repo.root().join("feature.txt")).unwrap();
    assert!(file.contains("<<<<<<<") && file.contains("edited, not committed"), "{file}");
}

#[test]
fn merging_with_set_aside_a_run_not_blocked_on_a_dirty_checkout_is_refused() {
    let w = world("merge-set-aside-not-waiting");
    let first = run(&w, &["--delivery", "merge-local"]);
    assert!(text(&first.stdout).contains("state: merged"), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let merged = store(&w).run_ids().unwrap().remove(0);
    let main = rev(w.repo.root(), "refs/heads/main");

    let repo = w.repo.root().to_str().unwrap().to_string();
    let out = orq(&w, &["run", "merge", &merged, "--set-aside", "--repo", &repo]);
    assert_ne!(out.status.code(), Some(0));
    assert!(text(&out.stderr).contains("this run is not waiting on a dirty checkout"), "{}", text(&out.stderr));
    assert_eq!(store(&w).run_ids().unwrap(), vec![merged], "nothing was resumed");
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), main);
}

/// Commits `file` on the repository's checked-out main, without pushing it.
fn commit_on_local_main(w: &World, file: &str, contents: &str) -> String {
    std::fs::write(w.repo.root().join(file), contents).unwrap();
    git(w.repo.root(), &["add", file]);
    git(w.repo.root(), &["-c", "user.name=Someone", "-c", "user.email=someone@example.invalid", "commit", "--quiet", "-m", &format!("local: {file}")]);
    rev(w.repo.root(), "refs/heads/main")
}

/// Has the implementer, on its first call, commit `file` to the local main in the project root: the
/// run started from main, and main then moves on while it works. The commit is found by its message.
fn local_main_moves_while_implementing(w: &World, file: &str, contents: &str) {
    let root = w.repo.root().display();
    let done = w.home.join("moved");
    let commit = format!(
        "[ -f '{done}' ] || {{ touch '{done}'; printf '%s' '{contents}' > '{root}/{file}'; git -C '{root}' add '{file}'; git -C '{root}' -c user.name=Someone -c user.email=someone@example.invalid commit --quiet -m 'local: {file}'; }}",
        done = done.display(),
        contents = contents.trim_end(),
    );
    executable(
        &w.home.join("bin/claude"),
        &format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) exit 0;; esac\n{commit}\nexec '{}' \"$@\"\n", w._agent.path().display()),
    );
}

fn moved_main(w: &World, file: &str) -> String {
    rev(w.repo.root(), &format!(":/local: {file}"))
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

/// Gate script that, on its first run only, commits `file` to the local main in the project root and
/// passes: the run's first gate is green, then the merge finds main moved and catches up with it.
fn main_moves_at_first_gate(w: &World, file: &str, contents: &str, then: &str) -> String {
    let root = w.repo.root().display();
    let done = w.home.join("moved-at-gate");
    format!(
        "[ -f '{done}' ] || {{ touch '{done}'; printf '%s' '{contents}' > '{root}/{file}'; git -C '{root}' add '{file}'; git -C '{root}' -c user.name=Someone -c user.email=someone@example.invalid commit --quiet -m 'local: {file}'; exit 0; }}; {then}",
        done = done.display(),
        contents = contents.trim_end(),
    )
}

fn gate_bases(seen: &Path) -> Vec<String> {
    std::fs::read_to_string(seen).unwrap_or_default().lines().map(str::to_string).collect()
}

#[test]
fn a_merge_local_run_whose_base_moved_before_its_gate_runs_the_gate_once_on_it_and_fast_forwards() {
    let w = world("merge-moved");
    let remote_main = rev(&w.remote, "refs/heads/main");
    // Local main moves on while the run works; the remote's main does not.
    local_main_moves_while_implementing(&w, "local.txt", "local\n");
    let seen = gate_that_records_its_base(&w, "moved", "true");

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "local.txt");

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    let head = rev(w.repo.root(), &format!("refs/heads/{}", record.branch));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), head, "main was fast-forwarded to the run's branch");
    assert_eq!(rev(w.repo.root(), &format!("{head}~1")), local_main, "the run's work sits on the local main it found");
    assert!(w.repo.root().join("local.txt").exists() && w.repo.root().join("feature.txt").exists(), "the checkout has both");
    assert_eq!(gate_bases(&seen), vec![local_main.clone()], "one gate, on the branch rebased onto the local main before it");
    let events = store(&w).events(&id).unwrap();
    let rebased: Vec<_> = events.iter().filter(|e| e.kind == "rebase.completed").collect();
    assert_eq!(rebased.len(), 1, "no catch-up round at the merge");
    assert_eq!(rebased[0].payload["target_sha"], local_main.as_str());
    assert_eq!(rebased[0].payload["source"], "local");
    assert_eq!(rev(&w.remote, "refs/heads/main"), remote_main, "nothing was pushed");
    assert_eq!(ending(&w, &id).as_deref(), Some("merged"));
}

#[test]
fn a_merge_local_run_whose_rebase_conflict_its_session_leaves_unresolved_is_blocked_with_main_left_alone() {
    let w = world("merge-moved-conflict");
    // Local main gets feature.txt too, with other contents: the rebase onto local main conflicts.
    local_main_moves_while_implementing(&w, "feature.txt", "theirs\n");
    let seen = gate_that_records_its_base(&w, "moved-conflict", "true");

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "feature.txt");

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("is still in progress after the fix session"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    let conflict = store(&w).events(&id).unwrap().into_iter().find(|e| e.kind == "rebase.conflict").expect("the conflict is recorded");
    assert_eq!(conflict.payload["where"], "before_gate", "the conflict with the local main is found before the gate");
    assert_eq!(conflict.payload["onto"], local_main.as_str());
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert!(gate_bases(&seen).is_empty(), "the gate is not run on a branch mid-rebase");
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
    local_main_moves_while_implementing(&w, "local.txt", "local\n");
    let seen = gate_that_records_its_base(&w, "moved-no-rebase", "true");

    let out = run(&w, &["--delivery", "merge-local", "--no-rebase"]);
    let local_main = moved_main(&w, "local.txt");

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
    // Green on the branch the run started from, red once local.txt is in it.
    let script = main_moves_at_first_gate(&w, "local.txt", "local\n", "test ! -e local.txt");
    let seen = gate_that_records_its_base(&w, "moved-red", &script);

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "local.txt");

    assert_eq!(out.status.code(), Some(31), "failed: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("the gate failed"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    assert_eq!(ending(&w, &id).as_deref(), Some("failed"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert!(gate_bases(&seen).len() >= 2, "the gate ran again after the catch-up");
}

/// Gives the run's gate-fix sessions `script` (sh, in the run's worktree) to run instead of doing nothing.
fn fixer(w: &World, script: &str) {
    executable(
        &w.home.join("bin/claude"),
        &format!("#!/bin/sh\ncase \"$*\" in *'local gate failed'*) {script}\nexit 0;; esac\nexec '{}' \"$@\"\n", w._agent.path().display()),
    );
}

/// A merge-local run whose gate is green until it catches up with a local main that adds
/// local.txt, and green again once a fix leaves fixed.txt in the worktree.
fn red_after_catching_up(w: &World, tag: &str) {
    let script = main_moves_at_first_gate(w, "local.txt", "local\n", "test -e fixed.txt || test ! -e local.txt");
    gate_that_records_its_base(w, tag, &script);
}

#[test]
fn a_merge_local_run_whose_gate_is_repaired_after_catching_up_fast_forwards_main_to_the_repair() {
    let w = world("merge-moved-fixed");
    red_after_catching_up(&w, "moved-fixed");
    fixer(&w, "echo fixed > fixed.txt && git add fixed.txt && git -c user.name=Fixer -c user.email=fixer@example.invalid commit --quiet -m fix");

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "local.txt");

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    let head = rev(w.repo.root(), &format!("refs/heads/{}", record.branch));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), head, "main was fast-forwarded to the repaired branch");
    assert_eq!(rev(w.repo.root(), &format!("{head}~2")), local_main, "the run's work and its repair sit on the local main");
    assert!(w.repo.root().join("fixed.txt").exists(), "the checkout has the repair");
    assert_eq!(ending(&w, &id).as_deref(), Some("merged"));
}

#[test]
fn a_merge_local_run_whose_gate_fails_for_an_unrelated_reason_after_catching_up_is_blocked_with_main_left_alone() {
    let w = world("merge-moved-unrelated");
    red_after_catching_up(&w, "moved-unrelated");
    fixer(&w, "echo 'local.txt breaks the gate on main already' > \"$ORQ_UNRELATED_FAILURE_FILE\"");

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "local.txt");

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("unrelated to this run"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert!(record.worktree.exists(), "the run's worktree is kept");
}

#[test]
fn a_merge_local_run_whose_repair_after_catching_up_is_not_committed_is_blocked_with_main_left_alone() {
    let w = world("merge-moved-uncommitted");
    red_after_catching_up(&w, "moved-uncommitted");
    fixer(&w, "echo fixed > fixed.txt");

    let out = run(&w, &["--delivery", "merge-local"]);
    let local_main = moved_main(&w, "local.txt");

    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("uncommitted changes"), "the reason is given: {}", text(&out.stderr));
    let id = store(&w).run_ids().unwrap().remove(0);
    let record = store(&w).load(&id).unwrap();
    assert_eq!(ending(&w, &id).as_deref(), Some("blocked"));
    assert_eq!(rev(w.repo.root(), "refs/heads/main"), local_main, "main did not move");
    assert!(record.worktree.join("fixed.txt").exists(), "the uncommitted repair is kept");
}

#[test]
fn a_merge_local_run_that_catches_up_with_a_base_adding_a_stack_reports_the_steps_its_delivered_head_passed() {
    let w = world("merge-moved-steps");
    // Local main adds a Node package the remote's main does not have: the run's first gate finds
    // nothing to check, the gate after catching up runs its test script.
    commit_on_local_main(&w, ".pnp.cjs", "");
    commit_on_local_main(&w, "package.json", "{\"scripts\": {\"test\": \"node test.js\"}}\n");
    let npm_runs = w.home.join("npm-runs");
    executable(&w.home.join("bin/npm"), &format!("#!/bin/sh\necho \"$*\" >> '{}'\n", npm_runs.display()));

    let out = run(&w, &["--delivery", "merge-local"]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert_eq!(std::fs::read_to_string(&npm_runs).unwrap_or_default(), "run test\n", "the gate after catching up ran the test script once");
    let id = store(&w).run_ids().unwrap().remove(0);
    let shown = orq(&w, &["review", "show", &id, "--json"]);
    assert!(shown.status.success(), "{}", text(&shown.stderr));
    let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        report["tests"]["commands"],
        serde_json::json!([{"command": "npm run test", "exit_code": 0}]),
        "the report lists the steps run on the delivered head: {report}"
    );
}

/// The web launcher runs `orq run merge <run> --set-aside --detach --in-herdr --repo <repo>`: it returns at once
/// naming the run that carries on, which merges on its own; without Herdr running it is detached as before.
#[test]
fn merging_with_set_aside_detached_in_herdr_names_the_run_that_carries_on_and_it_still_merges() {
    let (w, blocked) = blocked_on_a_dirty_checkout("merge-set-aside-detached");
    let repo = w.repo.root().to_str().unwrap().to_string();
    let out = orq(&w, &["run", "merge", &blocked, "--set-aside", "--detach", "--in-herdr", "--repo", &repo]);
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));

    let stdout = text(&out.stdout);
    let merged = stdout.lines().find_map(|line| line.strip_prefix("run: ")).expect("names the run that carries on").to_string();
    assert_ne!(merged, blocked, "{stdout}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    // The detached runner records the run as it starts, so it may not be there yet.
    let ended = |run: &str| store(&w).events(run).is_ok_and(|events| events.iter().any(|e| e.kind == "run.completed"));
    while !ended(&merged) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(ending(&w, &merged).as_deref(), Some("merged"));
}
