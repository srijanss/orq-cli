use std::collections::BTreeMap;

use orq_orchestrator::{carry_session_references, resumable_session, Event, RoleProfile, RunRecord, RunStore};

fn record(id: &str, harness: &str, model: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert("implementer".to_string(), RoleProfile { harness: harness.into(), model: model.into(), thinking: None });
    RunRecord {
        delivery: Default::default(),
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

fn started_at(run: &str, stage: &str, attempt: u32, session: &str) -> Event {
    let mut event = started(run, stage, session);
    event.event_id = format!("{run}-{stage}-{attempt}");
    event.attempt = attempt;
    event
}

#[test]
fn the_newest_session_of_the_same_harness_wins_over_an_older_one() {
    let store = store("newest");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    store.create(&record("run-2", "pi", "team/model")).unwrap();
    store.create(&record("run-3", "claude", "opus")).unwrap();
    store.append_event(&started_at("run-1", "implement_and_review", 1, "claude-old")).unwrap();
    store.append_event(&started_at("run-1", "gate_fix", 1, "claude-carried-newer")).unwrap();
    store.append_event(&started_at("run-2", "gate_fix", 1, "pi-newest")).unwrap();
    store.append_event(&started_at("run-3", "ci_fix", 1, "claude-local-newest")).unwrap();
    carry_session_references(&store, &store.load("run-2").unwrap(), "run-1").unwrap();
    carry_session_references(&store, &store.load("run-3").unwrap(), "run-2").unwrap();

    assert_eq!(resumable_session(&store, "run-3", "claude").unwrap().as_deref(), Some("claude-local-newest"));
    assert_eq!(resumable_session(&store, "run-3", "pi").unwrap().as_deref(), Some("pi-newest"));
}

#[test]
fn an_unknown_run_is_an_error_and_a_run_with_no_sessions_has_nothing_to_resume() {
    let store = store("errors");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    assert!(resumable_session(&store, "missing", "claude").is_err());
    assert_eq!(resumable_session(&store, "run-1", "claude").unwrap(), None);
}

#[test]
fn a_malformed_carried_history_is_an_error_not_a_fresh_start() {
    let store = store("malformed");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    let mut bad = started("run-1", "implement_and_review", "x");
    bad.kind = "run.sessions_carried".into();
    bad.payload = serde_json::json!({ "sessions": "not a list" });
    store.append_event(&bad).unwrap();
    assert!(resumable_session(&store, "run-1", "claude").is_err());
}

#[test]
fn a_newer_session_without_an_id_falls_back_to_the_latest_known_id_of_that_harness() {
    let store = store("missing-id");
    store.create(&record("run-1", "claude", "sonnet")).unwrap();
    store.append_event(&started_at("run-1", "implement_and_review", 1, "claude-known")).unwrap();
    let mut later = started_at("run-1", "gate_fix", 1, "unused");
    later.payload = serde_json::json!({ "session_id": null, "log": "/tmp/x.log" });
    store.append_event(&later).unwrap();
    assert_eq!(resumable_session(&store, "run-1", "claude").unwrap().as_deref(), Some("claude-known"));
}
