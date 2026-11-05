//! `orq run --check`: every check a launch makes, and nothing started.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

struct Env {
    state: PathBuf,
    config: PathBuf,
}

fn env(tag: &str) -> Env {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-check-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/orq")).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    Env { state: root.join("state"), config: root.join("config") }
}

fn run(repo: &Path, env: &Env, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--repo"])
        .arg(repo)
        .args(["--remote", "origin", "--target", "main"])
        .args(extra)
        .env("XDG_CONFIG_HOME", &env.config)
        .env("XDG_STATE_HOME", &env.state)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn state_is_empty(env: &Env) -> bool {
    std::fs::read_dir(&env.state).map(|mut entries| entries.next().is_none()).unwrap_or(true)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    text(&output.stdout)
}

#[test]
fn a_check_of_a_feature_that_may_start_says_ready_and_starts_nothing() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    let env = env("ready");

    let output = run(repo.root(), &env, &["--feature", "COM-01", "--check"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "state: ready\n");
    assert!(state_is_empty(&env), "nothing was recorded");
    assert_eq!(git(repo.root(), &["worktree", "list"]).lines().count(), 1, "no worktree was made");
}

#[test]
fn a_check_refuses_like_a_launch_would_a_feature_that_may_not_start_is_busy_or_badly_asked_for() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "completed"), LedgerEntry::new("COM-02", "pending")]);
    let env = env("refused");

    let ineligible = run(repo.root(), &env, &["--feature", "COM-01", "--check"]);
    assert_eq!(ineligible.status.code(), Some(20), "{}", text(&ineligible.stderr));
    for flags in [&["--feature", "COM-02", "--check", "--detach"][..], &["--feature", "COM-02", "--check", "--run-id", "run-1"][..]] {
        let output = run(repo.root(), &env, flags);
        assert_eq!(output.status.code(), Some(2), "{flags:?}: {}", text(&output.stderr));
    }
    assert!(state_is_empty(&env), "nothing was recorded");

    let project: serde_json::Value = serde_json::from_slice(
        &Command::new(env!("CARGO_BIN_EXE_orq")).args(["project", "inspect", "--cwd"]).arg(repo.root()).env_remove("HERDR_ACTIVE_PANE_CWD").output().unwrap().stdout,
    )
    .unwrap();
    let store = orq_orchestrator::RunStore::new(&env.state.join("orq"));
    store
        .start_run(&orq_orchestrator::RunRecord {
            run_id: "run-active".into(),
            project_id: project["id"].as_str().unwrap().to_string(),
            feature_id: "COM-02".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-active".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();

    let busy = run(repo.root(), &env, &["--feature", "COM-02", "--check"]);
    assert_eq!(busy.status.code(), Some(22), "{}", text(&busy.stderr));
    assert!(text(&busy.stderr).contains("already has an active run: run-active"), "{}", text(&busy.stderr));
    assert_eq!(store.run_ids().unwrap(), ["run-active"]);
}
