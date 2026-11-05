use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{request_pause, summarize_run, Event, RunPhase, RunRecord, RunStore};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-pause-req-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "COM-02".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: "fix--COM-02".into(),
            worktree: PathBuf::from("/w"),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store
}

fn add(store: &RunStore, kind: &str, stage: Option<&str>, payload: serde_json::Value) {
    let n = store.events("run-1").unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("e{n}"),
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "COM-02".into(),
            kind: kind.into(),
            stage: stage.map(str::to_string),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

fn phase(store: &RunStore) -> RunPhase {
    summarize_run(&store.events("run-1").unwrap()).phase
}

#[test]
fn only_a_running_run_can_be_asked_to_pause() {
    let s = store("running");
    add(
        &s,
        "stage.started",
        Some("implement"),
        serde_json::json!({}),
    );
    request_pause(&s, "run-1").unwrap();
    assert_eq!(phase(&s), RunPhase::PauseRequested);
    // Asking twice is refused, not silently recorded again.
    assert!(request_pause(&s, "run-1")
        .unwrap_err()
        .contains("pause_requested"));
    assert_eq!(
        s.events("run-1")
            .unwrap()
            .iter()
            .filter(|e| e.kind == "run.pause_requested")
            .count(),
        1
    );

    for (tag, kind, payload) in [
        ("failed", "run.failed", serde_json::json!({"reason": "x"})),
        ("blocked", "run.blocked", serde_json::json!({"reason": "x"})),
        (
            "done",
            "run.completed",
            serde_json::json!({"ending": "review_ready"}),
        ),
    ] {
        let s = store(tag);
        add(&s, kind, None, payload);
        let before = s.events("run-1").unwrap().len();
        let reason = request_pause(&s, "run-1").unwrap_err();
        assert!(reason.contains("only a running run"), "{tag}: {reason}");
        assert_eq!(
            s.events("run-1").unwrap().len(),
            before,
            "{tag}: nothing is recorded"
        );
    }
    assert!(request_pause(&s, "nope").is_err());
}

#[test]
fn a_requested_pause_survives_the_stages_that_finish_before_the_boundary() {
    let s = store("sticky");
    add(&s, "stage.started", Some("gate"), serde_json::json!({}));
    request_pause(&s, "run-1").unwrap();
    add(&s, "stage.completed", Some("gate"), serde_json::json!({}));
    let summary = summarize_run(&s.events("run-1").unwrap());
    assert_eq!(summary.phase, RunPhase::PauseRequested);
    assert_eq!(summary.stage.as_deref(), Some("gate"));
    // A paused run resumes to running, and a failure still ends a run that was asked to pause.
    add(&s, "run.paused", None, serde_json::json!({}));
    assert_eq!(phase(&s), RunPhase::Paused);
    add(&s, "run.resumed", None, serde_json::json!({}));
    assert_eq!(phase(&s), RunPhase::Running);
    request_pause(&s, "run-1").unwrap();
    add(
        &s,
        "stage.failed",
        Some("rebase"),
        serde_json::json!({"reason": "boom"}),
    );
    assert_eq!(phase(&s), RunPhase::Failed);
}
