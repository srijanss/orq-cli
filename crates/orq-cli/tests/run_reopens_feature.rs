//! A feature an agent marked `in_progress` goes back to `pending` when its run leaves nothing to
//! continue: the run ends blocked or failed without a commit, or the run is discarded. Otherwise the
//! feature could not be launched again until someone edited the ledger by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    bin: PathBuf,
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

/// The status the repository's ledger records for `name`.
fn status_of(repo: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
}

/// A repository with a pushed `main` and a pending COM-01 next to an in-progress COM-02, and a
/// `claude` that marks COM-01 `in_progress` in the main ledger, as the TDD server does when the
/// agent starts the feature, before it runs `agent`.
fn world(tag: &str, agent: &FakeAgent) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-reopen-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = root.join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "in_progress")]);
    let ledger = repo.root().join(".tdd-features.json");
    let claude = root.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nsed -i 's/\"pending\"/\"in_progress\"/' '{ledger}'\nexec '{agent}' \"$@\"\n",
            ledger = ledger.display(),
            agent = agent.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    World { repo, _remote: remote, state: root.join("state"), config: root.join("config"), ssh, bin: root.join("bin") }
}

fn github() -> FakeProvider {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
        .build()
}

fn run(w: &World, provider: &FakeProvider) -> Output {
    let mut dirs = vec![w.bin.clone(), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(w.repo.root())
        .args(["--feature", "COM-01", "--remote", "origin", "--target", "main", "--no-ci-check"])
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", std::env::join_paths(dirs).unwrap())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

#[test]
fn reopen_a_run_that_ends_blocked_without_a_commit_puts_its_feature_back_to_pending() {
    let agent = FakeAgent::builder().build();
    let w = world("no-commit", &agent);

    let output = run(&w, &github());

    assert!(text(&output.stdout).contains("blocked"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "pending", "{}", text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-02"), "in_progress", "a feature outside the run is left alone");
}

#[test]
fn reopen_a_run_that_committed_leaves_its_feature_as_the_agent_left_it() {
    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let w = world("committed", &agent);

    let output = run(&w, &github());

    assert!(text(&output.stdout).contains("state: review_ready"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "in_progress");
}

/// A failing run with `explain_failures` off, so no explainer session starts after it.
fn failing_world(tag: &str, agent: &FakeAgent) -> World {
    let w = world(tag, agent);
    std::fs::create_dir_all(w.repo.root().join(".orq")).unwrap();
    std::fs::write(w.repo.root().join(".orq/run.toml"), "explain_failures = false\n").unwrap();
    w
}

#[test]
fn reopen_a_run_that_fails_without_a_commit_puts_its_feature_back_to_pending() {
    let agent = FakeAgent::builder().exit_code(3).build();
    let w = failing_world("failed-no-commit", &agent);

    let output = run(&w, &github());

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "pending", "{}", text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-02"), "in_progress");
}

#[test]
fn reopen_a_run_that_fails_after_committing_leaves_its_feature_in_progress() {
    let agent = FakeAgent::builder().write_file("feature.txt", "half\n").commit("wip").exit_code(3).build();
    let w = failing_world("failed-committed", &agent);

    let output = run(&w, &github());

    assert!(text(&output.stdout).contains("failed"), "{}\n{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(status_of(w.repo.root(), "COM-01"), "in_progress", "the commit is work a resume continues");
}

fn event(kind: &str) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "COM-01".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload: serde_json::json!({}),
    }
}

/// The id `orq` files the runs of `repo` under.
fn project_id(repo: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    project["id"].as_str().unwrap().to_string()
}

struct Discard {
    repo: GitFixture,
    home: PathBuf,
    store: RunStore,
    project_id: String,
}

fn record(run_id: &str, project_id: &str, branch: &str, worktree: PathBuf) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        project_id: project_id.into(),
        feature_id: "COM-01".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "abc".into(),
        branch: branch.into(),
        worktree,
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

/// A blocked run-1 of COM-01, filed under the project of a repository whose ledger has COM-01 and
/// COM-02 in progress and COM-03 completed. `with_worktree` false leaves the run without a worktree.
fn blocked_run(tag: &str, with_worktree: bool) -> Discard {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-reopen-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[LedgerEntry::new("COM-01", "in_progress"), LedgerEntry::new("COM-02", "in_progress"), LedgerEntry::new("COM-03", "completed")],
    );
    let worktree = if with_worktree { repo.add_linked_worktree("orq-run-1") } else { home.join("gone") };
    let project_id = project_id(repo.root());
    let store = RunStore::new(&home.join("orq"));
    store.start_run(&record("run-1", &project_id, "orq-run-1", worktree)).unwrap();
    store.append_event(&event("stage.started")).unwrap();
    store.append_event(&event("run.blocked")).unwrap();
    Discard { repo, home, store, project_id }
}

fn discard(d: &Discard, repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "discard", "run-1", "--yes", "--repo"])
        .arg(repo)
        .env("XDG_STATE_HOME", &d.home)
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

#[test]
fn reopen_discarding_a_blocked_run_puts_its_feature_back_to_pending() {
    let d = blocked_run("discard", true);

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "pending");
    assert_eq!(status_of(d.repo.root(), "COM-02"), "in_progress", "a feature outside the run is left alone");
    assert_eq!(status_of(d.repo.root(), "COM-03"), "completed");
    assert!(text(&output.stdout).contains("COM-01") && text(&output.stdout).contains("pending"), "{}", text(&output.stdout));
}

#[test]
fn reopen_discarding_with_another_repository_leaves_that_ledger_alone() {
    let d = blocked_run("discard-elsewhere", false);
    let other = GitFixture::new();
    write_ledger(other.root(), &[LedgerEntry::new("COM-01", "in_progress")]);

    let output = discard(&d, other.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(other.root(), "COM-01"), "in_progress", "another project's feature of the same name is not reset");
    assert!(!text(&output.stdout).contains("pending"), "{}", text(&output.stdout));
}

#[test]
fn reopen_discarding_leaves_a_feature_another_run_now_holds_in_progress() {
    let d = blocked_run("discard-taken", true);
    d.store.finish_run(&d.project_id, "COM-01", "run-1").unwrap();
    d.store.start_run(&record("run-2", &d.project_id, "orq-run-2", d.home.join("w2"))).unwrap();

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "in_progress", "run-2 is working on it");
    assert_eq!(d.store.active_run(&d.project_id, "COM-01").unwrap().as_deref(), Some("run-2"));
}

#[test]
fn reopen_discarding_a_run_whose_feature_the_agent_marked_completed_puts_it_back_to_pending() {
    let d = blocked_run("discard-completed", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed"), LedgerEntry::new("COM-03", "completed")]);

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "pending", "the work it recorded as complete is gone with the run");
    assert_eq!(status_of(d.repo.root(), "COM-03"), "completed", "a feature outside the run is left alone");
    assert!(text(&output.stdout).contains("COM-01") && text(&output.stdout).contains("pending"), "{}", text(&output.stdout));
}

/// Another run of COM-01 in the project of `d`, whose lock is already released, started at `at`.
fn finished_run_of_the_feature(d: &Discard, run_id: &str, at: &str) {
    d.store.finish_run(&d.project_id, "COM-01", "run-1").unwrap();
    d.store.start_run(&record(run_id, &d.project_id, &format!("orq-{run_id}"), d.home.join(run_id))).unwrap();
    d.store.append_event(&Event { run_id: run_id.into(), occurred_at: at.into(), ..event("stage.started") }).unwrap();
    d.store.finish_run(&d.project_id, "COM-01", run_id).unwrap();
}

#[test]
fn reopen_discarding_an_old_run_leaves_a_feature_a_later_run_completed() {
    let d = blocked_run("discard-later-completed", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    finished_run_of_the_feature(&d, "run-2", "2026-11-02T11:00:00Z");

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "completed", "run-2 finished it after run-1");
    assert!(!text(&output.stdout).contains("pending"), "{}", text(&output.stdout));
}

#[test]
fn reopen_discarding_a_later_run_reopens_a_feature_an_older_run_also_worked_on() {
    let d = blocked_run("discard-older-run", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    finished_run_of_the_feature(&d, "run-0", "2026-11-02T09:00:00Z");

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "pending", "run-0 came before the discarded run, so the status is the discarded run's");
}

#[test]
fn reopen_discarding_leaves_a_completed_feature_when_another_run_cannot_be_read() {
    let d = blocked_run("discard-unreadable-run", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    let unreadable = d.home.join("orq/runs/run-9");
    std::fs::create_dir_all(&unreadable).unwrap();
    std::fs::write(unreadable.join("run.json"), "not json").unwrap();

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "completed", "a run that cannot be read may be the one that finished it");
    assert!(text(&output.stderr).contains("stay completed"), "{}", text(&output.stderr));
}

#[test]
fn reopen_discarding_counts_a_run_started_in_the_same_second_as_later() {
    let d = blocked_run("discard-same-second", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    finished_run_of_the_feature(&d, "run-2", "2026-11-02T10:00:00Z");

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "completed", "which of the two finished it cannot be told");
}

#[test]
fn reopen_discarding_counts_a_run_without_events_as_later() {
    let d = blocked_run("discard-eventless-other", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed")]);
    d.store.finish_run(&d.project_id, "COM-01", "run-1").unwrap();
    d.store.start_run(&record("run-2", &d.project_id, "orq-run-2", d.home.join("run-2"))).unwrap();
    d.store.finish_run(&d.project_id, "COM-01", "run-2").unwrap();

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "completed", "a run with no time may be the one that finished it");
}

#[test]
fn reopen_discarding_a_run_without_events_counts_every_other_run_as_later() {
    let d = blocked_run("discard-eventless-self", true);
    d.store.finish_run(&d.project_id, "COM-01", "run-1").unwrap();
    d.store.start_run(&record("run-2", &d.project_id, "orq-run-2", d.home.join("run-2"))).unwrap();

    let free = orq_orchestrator::features_no_later_run_took(&d.store, "run-2", &["COM-01".to_string()]).unwrap();

    assert!(free.is_empty(), "run-1 has a time and run-2 has none: {free:?}");
}

#[test]
fn reopen_discarding_a_group_keeps_only_the_member_a_later_run_took() {
    let d = blocked_run("discard-group-partial", true);
    write_ledger(d.repo.root(), &[LedgerEntry::new("COM-01", "completed"), LedgerEntry::new("COM-02", "completed")]);
    d.store
        .append_event(&Event { payload: serde_json::json!({ "features": ["COM-01", "COM-02"] }), ..event("run.grouped") })
        .unwrap();
    let later = RunRecord { feature_id: "COM-02".into(), ..record("run-2", &d.project_id, "orq-run-2", d.home.join("run-2")) };
    d.store.start_run(&later).unwrap();
    d.store
        .append_event(&Event { run_id: "run-2".into(), feature_id: "COM-02".into(), occurred_at: "2026-11-02T11:00:00Z".into(), ..event("stage.started") })
        .unwrap();
    d.store.finish_run(&d.project_id, "COM-02", "run-2").unwrap();

    let output = discard(&d, d.repo.root());

    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(status_of(d.repo.root(), "COM-01"), "pending", "nothing else took it");
    assert_eq!(status_of(d.repo.root(), "COM-02"), "completed", "run-2 finished it after the group");
}
