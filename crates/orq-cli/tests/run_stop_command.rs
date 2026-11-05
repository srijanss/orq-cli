use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunPhase, RunRecord, RunStore};

fn orq(state_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq")).args(args).env("XDG_STATE_HOME", state_home).output().unwrap()
}

fn seed(tag: &str) -> (PathBuf, RunStore) {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-stop-{}-{tag}", std::process::id()));
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
            kind: "stage.started".into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    (home, store)
}

#[test]
fn stopping_a_live_run_ends_its_owner_process_and_records_it_failed() {
    let (home, store) = seed("live");
    let mut owner = Command::new("sleep").arg("60").spawn().unwrap();
    orq_orchestrator::record_owner(&store, "run-1", owner.id()).unwrap();

    let out = orq(&home, &["run", "stop", "run-1"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!owner.wait().unwrap().success(), "the owner process was ended");
    let events = store.events("run-1").unwrap();
    assert_eq!(orq_orchestrator::summarize_run(&events).phase, RunPhase::Failed);

    let again = orq(&home, &["run", "stop", "run-1"]);
    assert!(!again.status.success(), "a stopped run cannot be stopped again");
    assert!(String::from_utf8_lossy(&again.stderr).contains("failed"), "{}", String::from_utf8_lossy(&again.stderr));
}

#[test]
fn stopping_an_unknown_run_or_with_a_bad_argument_is_refused() {
    let (home, store) = seed("bad");
    assert!(!orq(&home, &["run", "stop", "no-such-run"]).status.success());
    assert!(!orq(&home, &["run", "stop", "../x"]).status.success());
    assert!(!orq(&home, &["run", "stop"]).status.success());
    assert_eq!(store.events("run-1").unwrap().len(), 1);
}
