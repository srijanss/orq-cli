use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use orq_orchestrator::{request_pause, Event, RunRecord, RunStore};

fn orq(state_home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(args).stdin(Stdio::null());
    command.env("XDG_STATE_HOME", state_home);
    command.output().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn seed(tag: &str, pause: bool) -> (PathBuf, RunStore, PathBuf) {
    let home = orq_test_support::process_temp_dir().join(format!("orq-resume-cmd-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let wt = repo.join(".claude/worktrees/w1");
    git(&repo, &["worktree", "add", "-q", "-b", "orq/run-1", wt.to_str().unwrap()]);
    let store = RunStore::new(&home.join("orq"));
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: "orq/run-1".into(),
            worktree: wt,
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e0".into(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "stage.started".into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    if pause {
        request_pause(&store, "run-1").unwrap();
        assert!(orq_orchestrator::pause_at_boundary(&store, "run-1", &["implement"], "gate", None, None).unwrap());
    }
    (home, store, repo)
}

#[test]
fn without_confirmation_or_for_a_run_that_is_not_paused_nothing_changes() {
    let (home, store, repo) = seed("noyes", true);
    let before = store.events("run-1").unwrap().len();
    let out = orq(&home, &["run", "resume", "run-1", "--repo", repo.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert_eq!(store.events("run-1").unwrap().len(), before);

    let (home, store, repo) = seed("running", false);
    let out = orq(&home, &["run", "resume", "run-1", "--yes", "--repo", repo.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only a paused run"));
    assert_eq!(store.events("run-1").unwrap().len(), 1);
}

#[test]
fn an_invalid_checkpoint_is_refused_with_the_reason_and_the_run_stays_paused() {
    let (home, store, repo) = seed("moved", true);
    let wt = store.load("run-1").unwrap().worktree;
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "y"]);
    let before = store.events("run-1").unwrap().len();
    let out = orq(&home, &["run", "resume", "run-1", "--yes", "--repo", repo.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("HEAD moved"));
    assert_eq!(store.events("run-1").unwrap().len(), before);
}

#[test]
fn a_paused_run_that_a_later_run_of_the_feature_superseded_is_refused() {
    let (home, store, repo) = seed("stale", true);
    let mut later = store.load("run-1").unwrap();
    later.run_id = "run-2".into();
    later.branch = "orq/run-2".into();
    store.create(&later).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e1".into(),
            run_id: "run-2".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "stage.started".into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-02T11:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    let out = orq(&home, &["run", "resume", "run-1", "--yes", "--repo", repo.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("run-2"), "{}", String::from_utf8_lossy(&out.stderr));
}
