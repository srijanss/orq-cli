use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn orq(state_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq")).args(args).env("XDG_STATE_HOME", state_home).output().unwrap()
}

fn event(kind: &str) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload: serde_json::json!({}),
    }
}

struct Seeded {
    home: PathBuf,
    store: RunStore,
    repo: GitFixture,
    worktree: PathBuf,
}

fn seed(tag: &str, ending: Option<&str>) -> Seeded {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-discard-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    std::fs::write(worktree.join("scratch.txt"), "uncommitted work\n").unwrap();
    let store = RunStore::new(&home.join("orq"));
    store
        .start_run(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq-run-1".into(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store.append_event(&event("stage.started")).unwrap();
    if let Some(kind) = ending {
        store.append_event(&event(kind)).unwrap();
    }
    let logs = home.join("orq").join("logs").join("run-1");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("runner.log"), "log\n").unwrap();
    Seeded { home, store, repo, worktree }
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .status()
        .unwrap()
        .success()
}

#[test]
fn discarding_a_failed_run_removes_its_record_logs_worktree_branch_and_lock() {
    let s = seed("failed", Some("run.failed"));
    let repo = s.repo.root().to_str().unwrap();

    let dry = orq(&s.home, &["run", "discard", "run-1", "--repo", repo]);
    assert!(!dry.status.success(), "without --yes nothing is discarded");
    assert!(s.worktree.exists() && s.store.load("run-1").is_ok());
    std::fs::remove_file(s.worktree.join("scratch.txt")).unwrap();

    let out = orq(&s.home, &["run", "discard", "run-1", "--yes", "--repo", repo]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!s.worktree.exists(), "worktree removed");
    assert!(!branch_exists(s.repo.root(), "orq-run-1"), "branch removed");
    assert!(s.store.load("run-1").is_err(), "record removed");
    assert!(!s.home.join("orq").join("logs").join("run-1").exists(), "logs removed");
    assert_eq!(s.store.active_run("p", "F").unwrap(), None, "feature lock released");
}

#[test]
fn a_run_that_is_not_failed_or_a_bad_argument_discards_nothing() {
    let s = seed("running", None);
    let repo = s.repo.root().to_str().unwrap();
    for args in [
        vec!["run", "discard", "run-1", "--yes", "--repo", repo],
        vec!["run", "discard", "no-such-run", "--yes", "--repo", repo],
        vec!["run", "discard", "../x", "--yes", "--repo", repo],
        vec!["run", "discard", "--yes", "--repo", repo],
    ] {
        assert!(!orq(&s.home, &args).status.success(), "{args:?}");
    }
    assert!(s.worktree.exists() && branch_exists(s.repo.root(), "orq-run-1"));
    assert!(s.store.load("run-1").is_ok());
    assert_eq!(s.store.active_run("p", "F").unwrap().as_deref(), Some("run-1"));
}

#[test]
fn discard_keep_a_paused_run_with_uncommitted_work_is_discarded_and_says_where_its_worktree_was_kept() {
    let s = seed("paused-dirty", Some("run.pause_requested"));
    s.store.append_event(&event("run.paused")).unwrap();
    let repo = s.repo.root().to_str().unwrap();

    let out = orq(&s.home, &["run", "discard", "run-1", "--yes", "--repo", repo]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Discarded run run-1.") && stdout.contains(&format!("kept {}", s.worktree.display())), "{stdout}");
    assert!(s.worktree.join("scratch.txt").exists() && branch_exists(s.repo.root(), "orq-run-1"));
    assert!(s.store.load("run-1").is_err());
    assert_eq!(s.store.active_run("p", "F").unwrap(), None);
}
