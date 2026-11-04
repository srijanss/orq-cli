use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};

fn orq(state_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq")).args(args).env("XDG_STATE_HOME", state_home).output().unwrap()
}

fn seed(tag: &str, kind: &str, payload: serde_json::Value) -> (PathBuf, RunStore) {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-pause-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let store = RunStore::new(&home.join("orq"));
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-1".into(),
            worktree: PathBuf::from("/tmp/w"),
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
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload,
        })
        .unwrap();
    (home, store)
}

#[test]
fn pausing_a_running_run_records_the_request() {
    let (home, store) = seed("ok", "stage.started", serde_json::json!({}));
    let out = orq(&home, &["run", "pause", "run-1"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Pause requested"));
    assert!(store.events("run-1").unwrap().iter().any(|e| e.kind == "run.pause_requested"));
}

#[test]
fn pausing_a_finished_or_unknown_run_is_refused_and_records_nothing() {
    let (home, store) = seed("failed", "run.failed", serde_json::json!({"reason": "x"}));
    let out = orq(&home, &["run", "pause", "run-1"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only a running run can be paused"));
    assert_eq!(store.events("run-1").unwrap().len(), 1);

    assert!(!orq(&home, &["run", "pause", "../x"]).status.success());
    assert!(!orq(&home, &["run", "pause"]).status.success());
}
