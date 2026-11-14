//! An agent's TDD server writes the ledger itself: `in_progress` when it starts the feature and
//! `completed` when its cycle ends, both before the run is delivered. A run that ends without
//! delivering must not leave its feature `completed`, and resuming it must accept the feature as the
//! agent left it, or a failed run could never be picked up again.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    github: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A repository with a pushed `main`, a pending COM-01, and runs that do not explain their failures.
fn world(tag: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-resume-marked-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = base.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    World { repo, remote, state: base.join("state"), config: base.join("config"), ssh, base, github: github() }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

/// A PATH whose `claude` marks COM-01 `mark` in the main ledger, as the agent's TDD server does,
/// and then runs `agent`.
fn path_with(w: &World, agent: &FakeAgent, mark: &str, tag: &str) -> std::ffi::OsString {
    let bin = w.base.join(format!("bin-{tag}"));
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).unwrap();
    let ledger = w.repo.root().join(".tdd-features.json");
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\nsed -i -E 's/\"status\": \"[a-z_]+\"/\"status\": \"{mark}\"/' '{ledger}'\nexec '{agent}' \"$@\"\n",
            ledger = ledger.display(),
            agent = agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin, w.github.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

fn orq(w: &World, path: std::ffi::OsString, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .arg("--repo")
        .arg(w.repo.root())
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

/// `orq run` of COM-01 with a `claude` that marks it `mark` and then runs `agent`.
fn run(w: &World, agent: &FakeAgent, mark: &str, tag: &str) -> Output {
    let path = path_with(w, agent, mark, tag);
    orq(w, path, &["run", "--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
}

/// The status the repository's ledger records for `name`.
fn status_of(repo: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
}

/// An agent that commits half the work and then fails.
fn failing_agent() -> FakeAgent {
    FakeAgent::builder().write_file("feature.txt", "half\n").commit("wip").exit_code(3).build()
}

#[test]
fn a_failed_run_whose_agent_marked_its_feature_completed_leaves_it_in_progress() {
    let w = world("completed-then-failed");

    let output = run(&w, &failing_agent(), "completed", "first");

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "in_progress", "{}", text(&output.stderr));
}

#[test]
fn a_failed_run_with_no_commits_puts_a_feature_its_agent_marked_completed_back_to_pending() {
    let w = world("completed-no-commits");

    let output = run(&w, &FakeAgent::builder().exit_code(3).build(), "completed", "first");

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "pending", "{}", text(&output.stderr));
}

#[test]
fn a_delivered_run_leaves_the_feature_its_agent_marked_completed() {
    let w = world("delivered");
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat").build();

    let output = run(&w, &agent, "completed", "first");

    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed", "{}", text(&output.stderr));
}

/// The id of the only run recorded so far.
fn only_run(w: &World) -> String {
    let mut runs: Vec<String> = std::fs::read_dir(w.state.join("orq/runs")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

/// `orq run resume <run_id>` with a `claude` that marks the features `mark` and then runs `agent`.
fn resume(w: &World, run_id: &str, agent: &FakeAgent, mark: &str, tag: &str) -> Output {
    let path = path_with(w, agent, mark, tag);
    orq(w, path, &["run", "resume", run_id, "--yes", "--no-ci-check"])
}

fn finishing_agent() -> FakeAgent {
    FakeAgent::builder().write_file("again.txt", "rest\n").commit("feat: the rest").build()
}

/// A failed run of COM-01 whose agent marked it `mark` and committed `feature.txt` is resumed in its
/// worktree, and its work reaches the review branch.
fn assert_resumed_after_failing_marked(mark: &str) {
    let w = world(&format!("resume-{mark}"));
    let failed = run(&w, &failing_agent(), mark, "first");
    assert!(text(&failed.stdout).contains("failed"), "{}\n{}", text(&failed.stdout), text(&failed.stderr));
    let run_id = only_run(&w);

    let output = resume(&w, &run_id, &finishing_agent(), "in_progress", "second");

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{}\n{stdout}", text(&output.stderr));
    assert!(stdout.contains("state: review_ready"), "{stdout}");
    let pushed = git(&w.remote, &["ls-tree", "-r", "--name-only", "fix--COM-01"]);
    assert!(pushed.contains("feature.txt"), "the failed run's work was continued: {pushed}");
    assert!(pushed.contains("again.txt"), "{pushed}");
}

#[test]
fn a_failed_run_whose_agent_marked_its_feature_completed_is_resumed() {
    assert_resumed_after_failing_marked("completed");
}

#[test]
fn a_failed_run_whose_agent_marked_its_feature_in_progress_is_resumed() {
    assert_resumed_after_failing_marked("in_progress");
}

#[test]
fn a_fresh_run_of_a_feature_a_failed_run_left_in_progress_is_still_refused() {
    let w = world("fresh-refused");
    run(&w, &failing_agent(), "completed", "first");

    let output = run(&w, &finishing_agent(), "in_progress", "second");

    assert_eq!(output.status.code(), Some(20), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(text(&output.stderr).contains("may not start a run"), "{}", text(&output.stderr));
}

#[test]
fn a_failed_run_whose_feature_a_later_run_took_and_completed_is_not_resumed() {
    let w = world("later-took");
    write_ledger(w.repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
    run(&w, &failing_agent(), "completed", "first");
    let failed_run = only_run(&w);
    // Someone put the features back by hand, and a group run took COM-01 and delivered it.
    write_ledger(w.repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending")]);
    let group_agent = finishing_agent();
    let group = path_with(&w, &group_agent, "completed", "group");
    let delivered = orq(&w, group, &["run", "--group", "COM-01,COM-02", "--remote", "origin", "--target", "main", "--no-ci-check"]);
    assert!(text(&delivered.stdout).contains("state: review_ready"), "{}\n{}", text(&delivered.stdout), text(&delivered.stderr));

    let output = resume(&w, &failed_run, &finishing_agent(), "in_progress", "second");

    assert_ne!(output.status.code(), Some(0), "{}", text(&output.stdout));
    assert!(text(&output.stderr).contains("later run"), "{}", text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "completed");
}
