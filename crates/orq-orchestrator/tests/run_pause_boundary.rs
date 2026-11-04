use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use orq_orchestrator::{latest_checkpoint, pause_at_boundary, request_pause, summarize_run, Event, RunPhase, RunRecord, RunStore};

fn setup(tag: &str) -> (RunStore, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-pause-bnd-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]] {
        assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
    }
    let store = RunStore::new(&root.join("state"));
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
            worktree: repo.clone(),
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
    (store, repo)
}

#[test]
fn without_a_request_the_boundary_does_nothing() {
    let (store, _) = setup("none");
    assert!(!pause_at_boundary(&store, "run-1", &["implement"], "gate", None, None).unwrap());
    assert_eq!(store.events("run-1").unwrap().len(), 1);
}

#[test]
fn a_requested_pause_records_a_checkpoint_and_pauses_at_the_boundary() {
    let (store, _) = setup("req");
    request_pause(&store, "run-1").unwrap();
    assert!(pause_at_boundary(&store, "run-1", &["implement", "gate"], "push", None, None).unwrap());
    let events = store.events("run-1").unwrap();
    assert_eq!(summarize_run(&events).phase, RunPhase::Paused);
    let checkpoint = latest_checkpoint(&store, "run-1").unwrap().unwrap();
    assert_eq!(checkpoint.completed_stages, ["implement", "gate"]);
    assert_eq!(checkpoint.next_stage.as_deref(), Some("push"));
    assert_eq!(checkpoint.head_sha.len(), 40);
    // Already paused: a second boundary call is a no-op.
    assert!(!pause_at_boundary(&store, "run-1", &["implement"], "push", None, None).unwrap());
}
