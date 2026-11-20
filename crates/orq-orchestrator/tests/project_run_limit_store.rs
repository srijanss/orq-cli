//! A project's active runs, and a start that a project-wide limit refuses: the count and the start
//! are taken under one project lock, so two starts at once cannot both pass.

use std::collections::BTreeMap;

use orq_orchestrator::{Event, RunRecord, RunStore, StoreError};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!(
        "orq-project-run-limit-{}-{tag}",
        std::process::id()
    ));
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
fn only_running_pausing_and_paused_runs_are_active_and_a_start_at_the_limit_is_refused_with_them() {
    let store = store("count");
    let started = |run_id: &str, feature: &str| {
        let record = record("p", run_id, feature);
        store.start_run(&record).unwrap();
        event(&store, &record, "stage.started", serde_json::json!({}));
        record
    };
    let _running = started("run-a", "A");
    let paused = started("run-b", "B");
    event(
        &store,
        &paused,
        "run.pause_requested",
        serde_json::json!({}),
    );
    event(&store, &paused, "run.paused", serde_json::json!({}));
    let pausing = started("run-c", "C");
    event(
        &store,
        &pausing,
        "run.pause_requested",
        serde_json::json!({}),
    );
    for (run_id, feature, ending) in [
        ("run-d", "D", "review_ready"),
        ("run-e", "E", "failed"),
        ("run-f", "F", "needs_review"),
        ("run-j", "J", "blocked"),
    ] {
        let ended = started(run_id, feature);
        event(
            &store,
            &ended,
            "run.completed",
            serde_json::json!({"ending": ending}),
        );
    }
    // A paused run continued by a later run on its branch: its own events never say so.
    let taken = started("run-k", "K");
    event(&store, &taken, "run.pause_requested", serde_json::json!({}));
    event(&store, &taken, "run.paused", serde_json::json!({}));
    let later = RunRecord {
        run_id: "run-l".into(),
        worktree: "/w/elsewhere".into(),
        ..taken.clone()
    };
    store.create(&later).unwrap();
    event(&store, &later, "stage.started", serde_json::json!({}));
    event(
        &store,
        &later,
        "run.completed",
        serde_json::json!({"ending": "review_ready"}),
    );
    let other = record("q", "run-g", "G");
    store.start_run(&other).unwrap();

    let expected = vec![
        active("run-a", "A"),
        active("run-b", "B"),
        active("run-c", "C"),
    ];
    assert_eq!(
        store.active_project_runs("p").unwrap(),
        expected,
        "blocked, failed, review-waiting, ended and taken-over runs are not active"
    );
    assert_eq!(
        store.active_project_runs("q").unwrap(),
        vec![active("run-g", "G")],
        "a run with no events yet is active"
    );

    match store.start_group_run_limited(&record("p", "run-h", "H"), &["H".to_string()], Some(3)) {
        Err(StoreError::ProjectAtLimit { active }) => assert_eq!(active, expected),
        other => panic!("expected the limit to refuse the start, got {other:?}"),
    }
    assert!(store.load("run-h").is_err(), "no run was recorded");
    assert_eq!(
        store.active_run("p", "H").unwrap(),
        None,
        "no feature lock was kept"
    );

    store
        .start_group_run_limited(&record("p", "run-h", "H"), &["H".to_string()], Some(4))
        .unwrap();
    store
        .start_group_run_limited(&record("p", "run-i", "I"), &["I".to_string()], None)
        .unwrap();
    assert_eq!(store.active_project_runs("p").unwrap().len(), 5);
}

#[test]
fn of_starts_racing_at_a_limit_of_one_exactly_one_is_recorded() {
    let store = store("race");
    let starters = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(starters));
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-project-run-limit-{}-race", std::process::id()));
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
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "{outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Ok(()) | Err(StoreError::ProjectAtLimit { .. }))),
        "{outcomes:?}"
    );
    assert_eq!(store.run_ids().unwrap().len(), 1);
}

#[test]
fn resuming_a_run_holding_no_slot_takes_one_and_resuming_a_paused_or_pausing_one_keeps_the_slot_it_holds(
) {
    let store = store("resume-slot");
    let started = |run_id: &str| {
        let record = record("p", run_id, run_id);
        store.start_run(&record).unwrap();
        event(&store, &record, "stage.started", serde_json::json!({}));
        record
    };
    let histories: [(&str, Vec<(&str, serde_json::Value)>, bool); 5] = [
        (
            "run-blocked",
            vec![("run.completed", serde_json::json!({"ending": "blocked"}))],
            true,
        ),
        (
            "run-failed",
            vec![("run.failed", serde_json::json!({"reason": "x"}))],
            true,
        ),
        (
            "run-review",
            vec![(
                "run.completed",
                serde_json::json!({"ending": "needs_review"}),
            )],
            true,
        ),
        (
            "run-paused",
            vec![
                ("run.pause_requested", serde_json::json!({})),
                ("run.paused", serde_json::json!({})),
            ],
            false,
        ),
        (
            "run-pausing",
            vec![("run.pause_requested", serde_json::json!({}))],
            false,
        ),
    ];
    for (run_id, history, _) in &histories {
        let record = started(run_id);
        for (kind, payload) in history {
            event(&store, &record, kind, payload.clone());
        }
    }

    for (run_id, _, takes) in &histories {
        assert_eq!(
            orq_orchestrator::resume_takes_a_slot(&store, run_id),
            Ok(*takes),
            "{run_id}"
        );
    }
    assert!(
        orq_orchestrator::resume_takes_a_slot(&store, "run-missing").is_err(),
        "a run that cannot be read is not guessed at"
    );
}

#[test]
fn a_running_run_is_continued_only_by_a_later_run_of_its_own_project_on_its_branch_or_in_its_worktree(
) {
    let started = |run_id: &str, at: &str| Event {
        schema_version: 1,
        event_id: format!("{run_id}-0"),
        run_id: run_id.into(),
        project_id: String::new(),
        feature_id: String::new(),
        kind: "stage.started".into(),
        stage: None,
        attempt: 1,
        occurred_at: at.into(),
        payload: serde_json::json!({}),
    };
    let run = |run_id: &str, project: &str, branch: &str, worktree: &str, at: &str| {
        let record = RunRecord {
            branch: branch.into(),
            worktree: worktree.into(),
            ..record(project, run_id, run_id)
        };
        (run_id.to_string(), record, vec![started(run_id, at)])
    };
    let runs = vec![
        // Continued in its worktree by a later run on another branch.
        run("run-1", "p", "orq/a", "/w/a", "2026-11-01T00:00:00Z"),
        run("run-2", "p", "orq/a2", "/w/a", "2026-11-02T00:00:00Z"),
        // A later run of another project on the same branch and worktree continues nothing here.
        run("run-3", "p", "orq/b", "/w/b", "2026-11-01T00:00:00Z"),
        run("run-4", "q", "orq/b", "/w/b", "2026-11-02T00:00:00Z"),
        // An earlier run on the same branch does not continue a later one.
        run("run-5", "p", "orq/c", "/w/c1", "2026-11-01T00:00:00Z"),
        run("run-6", "p", "orq/c", "/w/c2", "2026-11-03T00:00:00Z"),
        // Runs with no recorded branch or worktree share nothing.
        run("run-7", "p", "", "", "2026-11-01T00:00:00Z"),
        run("run-8", "p", "", "", "2026-11-02T00:00:00Z"),
    ];
    let active: Vec<String> = orq_orchestrator::active_runs(&runs)
        .into_iter()
        .map(|(run_id, _)| run_id)
        .collect();
    assert_eq!(
        active,
        ["run-2", "run-3", "run-4", "run-6", "run-7", "run-8"]
    );
}
