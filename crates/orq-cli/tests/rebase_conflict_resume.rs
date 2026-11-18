//! A run blocked on a rebase conflict can be resumed: after a person finished the rebase by hand,
//! or with `--fix-conflicts`, which lets a session of its implementer try again.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::RunStore;
use orq_test_support::{write_ledger, FakeAgent, GitFixture, LedgerEntry};

struct World {
    repo: GitFixture,
    home: PathBuf,
    ssh: PathBuf,
    path: std::ffi::OsString,
    agent: FakeAgent,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// The words the conflict-fix session's task starts with; the fake harness knows it by them.
const FIX_TASK: &str = "The rebase of this run's branch onto";

/// Resolves the conflict on feature.txt and finishes the rebase.
const RESOLVE: &str = "printf 'merged\\n' > feature.txt && git add feature.txt && GIT_EDITOR=true git -c user.name=t -c user.email=t@example.invalid rebase --continue";

fn world(tag: &str) -> World {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let home = orq_test_support::process_temp_dir().join(format!("orq-rebase-resume-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let ssh = home.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    let mut dirs = vec![home.join("bin")];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "gate = [\"true\"]\n").unwrap();
    World { repo, home, ssh, path, agent }
}

/// The fake harness: a notification is answered at once; the implement session commits feature.txt after `on_implement` ran; a
/// conflict-fix session runs `on_fix` (sh) in the worktree and leaves its task in `fix-task.txt`.
fn harness(w: &World, on_implement: &str, on_fix: &str) {
    let task = w.home.join("fix-task.txt");
    executable(
        &w.home.join("bin/claude"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'local gate failed'*|*PushNotification*) exit 0;;\n*\"{FIX_TASK}\"*) printf '%s' \"$*\" > '{task}'; {on_fix}; exit 0;;\nesac\n{on_implement}\nexec '{agent}' \"$@\"\n",
            task = task.display(),
            agent = w.agent.path().display(),
        ),
    );
}

/// Shell that, once, commits feature.txt with other contents on the repository's local main, as
/// another run's local merge does while this one works.
fn local_main_takes_feature(w: &World) -> String {
    let done = w.home.join("moved");
    let root = w.repo.root().display().to_string();
    format!(
        "[ -f '{done}' ] || {{ touch '{done}'; printf 'theirs\\n' > '{root}/feature.txt' && git -C '{root}' add feature.txt && git -C '{root}' -c user.name=Someone -c user.email=someone@example.invalid commit -q -m 'other: feature.txt'; }}",
        done = done.display(),
    )
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

fn run(w: &World, extra: &[&str]) -> Output {
    let repo = w.repo.root().to_str().unwrap().to_string();
    let mut args = vec!["run", "--repo", &repo, "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check", "--delivery", "merge-local"];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn resume(w: &World, id: &str, extra: &[&str]) -> Output {
    let repo = w.repo.root().to_str().unwrap().to_string();
    let mut args = vec!["run", "resume", id, "--yes", "--repo", &repo];
    args.extend_from_slice(extra);
    orq(w, &args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn store(w: &World) -> RunStore {
    RunStore::new(&w.home.join("state/orq"))
}

fn only_run(w: &World) -> String {
    let ids = store(w).run_ids().unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids[0].clone()
}

fn events(w: &World, id: &str) -> Vec<orq_orchestrator::Event> {
    store(w).events(id).unwrap()
}

fn worktree(w: &World, id: &str) -> PathBuf {
    store(w).load(id).unwrap().worktree
}

/// The one run other than `other_than`.
fn other_run(w: &World, other_than: &str) -> String {
    let ids: Vec<String> = store(w).run_ids().unwrap().into_iter().filter(|id| id != other_than).collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids[0].clone()
}

/// A run whose conflict-fix session left the rebase unresolved: blocked, its worktree mid-rebase.
fn blocked_mid_rebase(w: &World) -> String {
    harness(w, &local_main_takes_feature(w), "true");
    let out = run(w, &[]);
    assert_eq!(out.status.code(), Some(30), "blocked: {}\n{}", text(&out.stdout), text(&out.stderr));
    only_run(w)
}

fn conflict_onto(w: &World, id: &str) -> serde_json::Value {
    events(w, id).iter().find(|e| e.kind == "rebase.conflict").expect("a rebase.conflict event").payload["onto"].clone()
}

#[test]
fn a_run_whose_rebase_a_person_finished_resumes_from_the_gate_and_merges() {
    let w = world("by-hand");
    let id = blocked_mid_rebase(&w);
    let onto = conflict_onto(&w, &id);
    let finished = Command::new("sh").arg("-c").arg(RESOLVE).current_dir(worktree(&w, &id)).status().unwrap();
    assert!(finished.success());

    let out = resume(&w, &id, &[]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let resumed = other_run(&w, &id);
    let events = events(&w, &resumed);
    let by_hand = events
        .iter()
        .find(|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "person")
        .expect("the person's rebase is recorded");
    assert_eq!(by_hand.payload["target_sha"], onto);
    assert!(events.iter().any(|e| e.kind == "stage.started" && e.stage.as_deref() == Some("verify")), "the gate ran");
    let main = git(w.repo.root(), &["rev-parse", "refs/heads/main"]);
    assert_eq!(git(w.repo.root(), &["show", &format!("{main}:feature.txt")]), "merged");
}

#[test]
fn a_run_still_mid_rebase_is_refused_with_the_ways_to_finish_it() {
    let w = world("mid-rebase");
    let id = blocked_mid_rebase(&w);

    let out = resume(&w, &id, &[]);

    assert_ne!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let said = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(said.contains("git rebase --continue"), "{said}");
    assert!(said.contains(&format!("orq run resume {id} --fix-conflicts")), "{said}");
    assert_eq!(store(&w).run_ids().unwrap(), vec![id], "no run was started");
}

#[test]
fn fix_conflicts_lets_a_session_resolve_the_rebase_and_the_run_goes_on_to_merge() {
    let w = world("fix-resolves");
    let id = blocked_mid_rebase(&w);
    harness(&w, "true", RESOLVE);

    let out = resume(&w, &id, &["--fix-conflicts"]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let resumed = other_run(&w, &id);
    let events = events(&w, &resumed);
    assert!(
        events.iter().any(|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "session"),
        "the session's rebase is recorded: {:?}",
        events.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
    assert!(std::fs::read_to_string(w.home.join("fix-task.txt")).unwrap().contains(FIX_TASK));
    let main = git(w.repo.root(), &["rev-parse", "refs/heads/main"]);
    assert_eq!(git(w.repo.root(), &["show", &format!("{main}:feature.txt")]), "merged");
}

#[test]
fn fix_conflicts_whose_session_leaves_the_rebase_unresolved_keeps_the_run_blocked() {
    let w = world("fix-fails");
    let id = blocked_mid_rebase(&w);
    std::fs::remove_file(w.home.join("fix-task.txt")).unwrap();

    let out = resume(&w, &id, &["--fix-conflicts"]);

    assert_eq!(out.status.code(), Some(30), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(w.home.join("fix-task.txt").exists(), "the session ran");
    let resumed = other_run(&w, &id);
    let events = events(&w, &resumed);
    assert!(events.iter().any(|e| e.kind == "run.blocked"));
    assert!(!events.iter().any(|e| e.kind == "rebase.completed"));
    assert!(orq_orchestrator::summarize_run(&events).phase == orq_orchestrator::RunPhase::Blocked);
}

#[test]
fn fix_conflicts_on_a_run_that_is_not_mid_rebase_is_refused() {
    let w = world("fix-not-mid-rebase");
    let id = blocked_mid_rebase(&w);
    let finished = Command::new("sh").arg("-c").arg(RESOLVE).current_dir(worktree(&w, &id)).status().unwrap();
    assert!(finished.success());

    let out = resume(&w, &id, &["--fix-conflicts"]);

    assert_ne!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains("--fix-conflicts"), "{}", text(&out.stderr));
    assert_eq!(store(&w).run_ids().unwrap(), vec![id], "no run was started");
}

/// The newest run: the one started last.
fn newest_run(w: &World) -> String {
    let store = store(w);
    store
        .run_ids()
        .unwrap()
        .into_iter()
        .max_by_key(|id| store.events(id).unwrap().first().map(|e| e.occurred_at.clone()).unwrap_or_default())
        .unwrap()
}

#[test]
fn a_resume_whose_fix_session_left_the_rebase_unresolved_can_be_fixed_again() {
    let w = world("fix-again");
    let first = blocked_mid_rebase(&w);
    let out = resume(&w, &first, &["--fix-conflicts"]);
    assert_eq!(out.status.code(), Some(30), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let second = other_run(&w, &first);
    harness(&w, "true", RESOLVE);

    let out = resume(&w, &second, &["--fix-conflicts"]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let third = newest_run(&w);
    assert!(events(&w, &third).iter().any(|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "session"));
}

#[test]
fn a_resume_whose_fix_session_left_the_rebase_unresolved_resumes_once_a_person_finished_it() {
    let w = world("fix-then-hand");
    let first = blocked_mid_rebase(&w);
    let onto = conflict_onto(&w, &first);
    let out = resume(&w, &first, &["--fix-conflicts"]);
    assert_eq!(out.status.code(), Some(30), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let second = other_run(&w, &first);
    let finished = Command::new("sh").arg("-c").arg(RESOLVE).current_dir(worktree(&w, &second)).status().unwrap();
    assert!(finished.success());

    let out = resume(&w, &second, &[]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("state: merged"), "{}", text(&out.stdout));
    let third = newest_run(&w);
    let by_hand = events(&w, &third).into_iter().find(|e| e.kind == "rebase.completed" && e.payload["resolved_by"] == "person");
    assert_eq!(by_hand.expect("the person's rebase is recorded").payload["target_sha"], onto);
}

#[test]
fn fix_conflicts_on_a_different_rebase_than_the_one_the_run_blocked_on_is_refused() {
    let w = world("fix-replaced-rebase");
    let id = blocked_mid_rebase(&w);
    let branch = store(&w).load(&id).unwrap().branch;
    let onto = conflict_onto(&w, &id);
    let onto = onto.as_str().unwrap();
    // The rebase the run blocked on is aborted, and the branch rebased onto another commit instead.
    let replaced = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "set -e; g='git -c user.name=t -c user.email=t@example.invalid'; $g rebase --abort; $g switch -q -c elsewhere {onto}; \
             printf 'other\\n' > feature.txt; $g commit -q -am elsewhere; $g switch -q '{branch}'; ! $g rebase elsewhere"
        ))
        .current_dir(worktree(&w, &id))
        .status()
        .unwrap();
    assert!(replaced.success());

    let out = resume(&w, &id, &["--fix-conflicts"]);

    assert_ne!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stderr).contains(onto), "{}", text(&out.stderr));
    assert_eq!(store(&w).run_ids().unwrap(), vec![id], "no run was started");
}

/// A shell line appending an `init_feature` event for `name` to the session log of the worktree the
/// session runs in, as the outside-in-tdd server writes it.
fn log_init(name: &str) -> String {
    format!("printf '%s\\n' '{{\"event\": \"init_feature\", \"featureName\": \"{name}\"}}' >> .tdd-session.log")
}

/// The repository ignores the session log, as a real project does. Returns a shell line that puts
/// the ledger as the sessions leave it over the repository's: COM-01 in progress, `impl-extra`
/// completed and `rfix-extra` in progress.
fn sessions_log_their_features(w: &World) -> String {
    let exclude = w.repo.root().join(".git/info/exclude");
    let mut ignored = std::fs::read_to_string(&exclude).unwrap_or_default();
    ignored.push_str("\n.tdd-session.log\n");
    std::fs::write(&exclude, ignored).unwrap();
    let dir = w.home.join("ledger");
    std::fs::create_dir_all(&dir).unwrap();
    let after = write_ledger(
        &dir,
        &[LedgerEntry::new("COM-01", "in_progress"), LedgerEntry::new("impl-extra", "completed"), LedgerEntry::new("rfix-extra", "in_progress")],
    );
    format!("cp '{}' '{}'", after.display(), w.repo.root().join(".tdd-features.json").display())
}

/// The features each `ledger.extra_features` event of `id` credits, in order.
fn credited(w: &World, id: &str) -> Vec<serde_json::Value> {
    events(w, id).into_iter().filter(|e| e.kind == "ledger.extra_features").map(|e| e.payload["features"].clone()).collect()
}

#[test]
fn a_rebase_fix_session_that_resolves_the_conflict_is_credited_with_the_feature_it_started_once() {
    let w = world("credit-resolved");
    let ledger = sessions_log_their_features(&w);
    let on_implement = format!("{}; {}; {ledger}", local_main_takes_feature(&w), log_init("impl-extra"));
    let on_fix = format!("{}; {}; {RESOLVE}", log_init("impl-extra"), log_init("rfix-extra"));
    harness(&w, &on_implement, &on_fix);

    let out = run(&w, &[]);

    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(w.home.join("fix-task.txt").exists(), "the rebase fix session ran");
    let id = only_run(&w);
    assert_eq!(credited(&w, &id), vec![serde_json::json!(["impl-extra"]), serde_json::json!(["rfix-extra"])]);
    assert!(text(&out.stderr).contains("the session also added: impl-extra, rfix-extra"), "{}", text(&out.stderr));
}

#[test]
fn a_rebase_fix_session_that_leaves_the_conflict_is_still_credited_with_the_feature_it_started() {
    let w = world("credit-blocked");
    let ledger = sessions_log_their_features(&w);
    harness(&w, &format!("{}; {ledger}", local_main_takes_feature(&w)), &log_init("rfix-extra"));

    let out = run(&w, &[]);

    assert_eq!(out.status.code(), Some(30), "{}\n{}", text(&out.stdout), text(&out.stderr));
    let id = only_run(&w);
    assert_eq!(credited(&w, &id), vec![serde_json::json!(["rfix-extra"])]);
    assert!(text(&out.stderr).contains("the session also added: rfix-extra"), "{}", text(&out.stderr));
}

/// One `init_feature` line of a session log, as the outside-in-tdd server writes it.
fn init_line(name: &str) -> String {
    format!("{{\"event\": \"init_feature\", \"featureName\": \"{name}\"}}")
}

/// A world whose project keeps its session log in git, with main holding an earlier session's
/// `old-extra` event. Returns it with the implement session's shell, which, once, lands another run's
/// merge on main that changes feature.txt and logs its session's `up-extra`, then starts `impl-extra`;
/// and the fix session's shell, which keeps both sides of the log, starts `rfix-extra` and resolves.
fn tracked_log_world(tag: &str) -> (World, String, String) {
    let w = world(tag);
    let root = w.repo.root();
    std::fs::write(root.join(".tdd-session.log"), format!("{}\n", init_line("old-extra"))).unwrap();
    git(root, &["add", ".tdd-session.log"]);
    git(root, &["commit", "-q", "-m", "keep the session log"]);
    let dir = w.home.join("ledger");
    std::fs::create_dir_all(&dir).unwrap();
    let after = write_ledger(
        &dir,
        &[
            LedgerEntry::new("COM-01", "in_progress"),
            LedgerEntry::new("old-extra", "completed"),
            LedgerEntry::new("up-extra", "completed"),
            LedgerEntry::new("impl-extra", "completed"),
            LedgerEntry::new("rfix-extra", "in_progress"),
        ],
    );
    let done = w.home.join("moved");
    let upstream = format!(
        "[ -f '{done}' ] || {{ touch '{done}'; printf 'theirs\\n' > '{r}/feature.txt' && printf '%s\\n' '{up}' >> '{r}/.tdd-session.log' && git -C '{r}' add feature.txt .tdd-session.log && git -C '{r}' -c user.name=Someone -c user.email=someone@example.invalid commit -q -m 'other: feature.txt'; }}",
        done = done.display(),
        r = root.display(),
        up = init_line("up-extra"),
    );
    let on_implement = format!("{upstream}; {}; cp '{}' '{}'", log_init("impl-extra"), after.display(), root.join(".tdd-features.json").display());
    let on_fix = format!(
        "printf '%s\\n' '{}' '{}' '{}' > .tdd-session.log; {}; git add .tdd-session.log; {RESOLVE}",
        init_line("old-extra"),
        init_line("up-extra"),
        init_line("impl-extra"),
        log_init("rfix-extra"),
    );
    (w, on_implement, on_fix)
}

#[test]
fn session_log_lines_a_rebase_brings_from_the_target_branch_are_not_credited_to_the_run() {
    let (w, on_implement, on_fix) = tracked_log_world("credit-tracked-log");
    harness(&w, &on_implement, &on_fix);

    let out = run(&w, &[]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{}\n{stderr}", text(&out.stdout));
    assert!(w.home.join("fix-task.txt").exists(), "the rebase fix session ran");
    let id = only_run(&w);
    assert_eq!(credited(&w, &id), vec![serde_json::json!(["impl-extra"]), serde_json::json!(["rfix-extra"])], "{stderr}");
    assert!(!stderr.contains("old-extra") && !stderr.contains("up-extra"), "{stderr}");
}

#[test]
fn a_resumed_run_does_not_credit_the_session_log_lines_its_predecessors_rebase_brought_in() {
    let (w, on_implement, on_fix) = tracked_log_world("credit-resumed-log");
    harness(&w, &on_implement, "true");
    let first = run(&w, &[]);
    assert_eq!(first.status.code(), Some(30), "{}\n{}", text(&first.stdout), text(&first.stderr));
    let id = only_run(&w);
    harness(&w, "true", &on_fix);

    let out = resume(&w, &id, &["--fix-conflicts"]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{}\n{stderr}", text(&out.stdout));
    let resumed = other_run(&w, &id);
    assert_eq!(credited(&w, &resumed), vec![serde_json::json!(["impl-extra", "rfix-extra"])], "{stderr}");
    assert!(!stderr.contains("old-extra") && !stderr.contains("up-extra"), "{stderr}");
}
