use std::collections::BTreeMap;

use orq_orchestrator::{carry_session_references, resumable_session, Event, RoleProfile, RunRecord, RunStore};

fn record(id: &str, harness: &str, model: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert("implementer".to_string(), RoleProfile { harness: harness.into(), model: model.into(), thinking: None });
    RunRecord {
        run_id: id.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: "orq/x".into(),
        worktree: std::path::PathBuf::from("/tmp/none"),
        repository: String::new(),
        profiles,
    }
}

fn started(run: &str, stage: &str, session: &str) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("{run}-{stage}"),
        run_id: run.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: "stage.started".into(),
        stage: Some(stage.into()),
        attempt: 1,
        occurred_at: "2026-11-05T10:00:00Z".into(),
        payload: serde_json::json!({ "session_id": session, "log": "/tmp/x.log" }),
    }
}

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-no-share-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    RunStore::new(&dir)
}

#[test]
fn a_session_id_is_only_offered_to_the_harness_that_produced_it() {
    let store = store("claude-to-pi");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    store.create(&record("run-2", "pi", "team/model")).unwrap();
    store.append_event(&started("run-1", "implement_and_review", "claude-sess-1")).unwrap();
    carry_session_references(&store, &store.load("run-2").unwrap(), "run-1").unwrap();

    assert_eq!(resumable_session(&store, "run-2", "pi").unwrap(), None, "pi must start fresh, not from claude's session");
    assert_eq!(resumable_session(&store, "run-2", "claude").unwrap().as_deref(), Some("claude-sess-1"));
}

#[test]
fn the_latest_session_of_the_same_harness_wins_over_a_newer_one_of_another_harness() {
    let store = store("latest");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    store.create(&record("run-2", "pi", "team/model")).unwrap();
    store.create(&record("run-3", "claude", "opus")).unwrap();
    store.append_event(&started("run-1", "implement_and_review", "claude-sess-1")).unwrap();
    store.append_event(&started("run-2", "gate_fix", "pi-sess-2")).unwrap();
    carry_session_references(&store, &store.load("run-2").unwrap(), "run-1").unwrap();
    carry_session_references(&store, &store.load("run-3").unwrap(), "run-2").unwrap();

    assert_eq!(resumable_session(&store, "run-3", "pi").unwrap().as_deref(), Some("pi-sess-2"));
    assert_eq!(resumable_session(&store, "run-3", "claude").unwrap().as_deref(), Some("claude-sess-1"));
}
