//! A project's active runs, and a start that a project-wide limit refuses: the count and the start
//! are taken under one project lock, so two starts at once cannot both pass.

use std::collections::BTreeMap;

use orq_orchestrator::{Event, RunRecord, RunStore, StoreError};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!("orq-project-run-limit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    RunStore::new(&root)
}

fn record(project: &str, run_id: &str, feature: &str) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.into(),
        project_id: project.into(),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: format!("orq/{run_id}"),
        worktree: format!("/w/{run_id}").into(),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn event(store: &RunStore, record: &RunRecord, kind: &str, payload: serde_json::Value) {
    let n = store.events(&record.run_id).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{}-{n}", record.run_id),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-18T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

/// An active run as the store names it: its id and its feature.
fn active(run_id: &str, feature: &str) -> (String, String) {
    (run_id.into(), feature.into())
}

#[test]
fn running_paused_and_blocked_runs_are_active_and_a_start_at_the_limit_is_refused_with_them() {
    let store = store("count");
    let started = |run_id: &str, feature: &str| {
        let record = record("p", run_id, feature);
        store.start_run(&record).unwrap();
        event(&store, &record, "stage.started", serde_json::json!({}));
        record
    };
    let _running = started("run-a", "A");
    let paused = started("run-b", "B");
    event(&store, &paused, "run.pause_requested", serde_json::json!({}));
    event(&store, &paused, "run.paused", serde_json::json!({}));
    let blocked = started("run-c", "C");
    event(&store, &blocked, "run.completed", serde_json::json!({"ending": "blocked"}));
    for (run_id, feature, ending) in [("run-d", "D", "review_ready"), ("run-e", "E", "failed"), ("run-f", "F", "needs_review")] {
        let ended = started(run_id, feature);
        event(&store, &ended, "run.completed", serde_json::json!({"ending": ending}));
    }
    let other = record("q", "run-g", "G");
    store.start_run(&other).unwrap();

    let expected = vec![active("run-a", "A"), active("run-b", "B"), active("run-c", "C")];
    assert_eq!(store.active_project_runs("p").unwrap(), expected);
    assert_eq!(store.active_project_runs("q").unwrap(), vec![active("run-g", "G")], "a run with no events yet is active");

    match store.start_group_run_limited(&record("p", "run-h", "H"), &["H".to_string()], Some(3)) {
        Err(StoreError::ProjectAtLimit { active }) => assert_eq!(active, expected),
        other => panic!("expected the limit to refuse the start, got {other:?}"),
    }
    assert!(store.load("run-h").is_err(), "no run was recorded");
    assert_eq!(store.active_run("p", "H").unwrap(), None, "no feature lock was kept");

    store.start_group_run_limited(&record("p", "run-h", "H"), &["H".to_string()], Some(4)).unwrap();
    store.start_group_run_limited(&record("p", "run-i", "I"), &["I".to_string()], None).unwrap();
    assert_eq!(store.active_project_runs("p").unwrap().len(), 5);
}

#[test]
fn of_starts_racing_at_a_limit_of_one_exactly_one_is_recorded() {
    let store = store("race");
    let starters = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(starters));
    let root = orq_test_support::process_temp_dir().join(format!("orq-project-run-limit-{}-race", std::process::id()));
    let outcomes: Vec<Result<(), StoreError>> = (0..starters)
        .map(|n| {
            let (root, barrier) = (root.clone(), barrier.clone());
            std::thread::spawn(move || {
                let store = RunStore::new(&root);
                let feature = format!("F{n}");
                let run = record("p", &format!("run-{n}"), &feature);
                barrier.wait();
                store.start_group_run_limited(&run, &[feature], Some(1))
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|starter| starter.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1, "{outcomes:?}");
    assert!(outcomes.iter().all(|outcome| matches!(outcome, Ok(()) | Err(StoreError::ProjectAtLimit { .. }))), "{outcomes:?}");
    assert_eq!(store.run_ids().unwrap().len(), 1);
}
