//! The machine's active runs, across every project, and a start that a machine-wide limit refuses:
//! the count and the start are taken under one machine lock, so two starts at once in different
//! projects cannot both pass.

use std::collections::BTreeMap;

use orq_orchestrator::{
    machine_disk_refusal, machine_limit_refusal, Event, FreeDisk, RunLimits, RunRecord, RunStore,
    StoreError,
};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!(
        "orq-machine-run-limit-{}-{tag}",
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

fn started(store: &RunStore, project: &str, run_id: &str, feature: &str) -> RunRecord {
    let record = record(project, run_id, feature);
    store.start_run(&record).unwrap();
    event(store, &record, "stage.started", serde_json::json!({}));
    record
}

fn pair(project: &str, run_id: &str) -> (String, String) {
    (project.into(), run_id.into())
}

#[test]
fn the_machine_counts_active_runs_of_every_project_and_refuses_a_start_at_its_limit_with_them() {
    let store = store("count");
    started(&store, "orq-cli-11111111", "run-a", "A");
    started(&store, "giftcard-display-22222222", "run-b", "A");
    let ended = started(&store, "orq-cli-11111111", "run-c", "C");
    event(
        &store,
        &ended,
        "run.completed",
        serde_json::json!({"ending": "review_ready"}),
    );
    let expected = vec![
        pair("orq-cli-11111111", "run-a"),
        pair("giftcard-display-22222222", "run-b"),
    ];
    assert_eq!(store.active_machine_runs().unwrap(), expected);

    let limits = RunLimits {
        project: None,
        machine: Some(2),
    };
    match store.start_group_run_within(
        &record("third-33333333", "run-d", "D"),
        &["D".to_string()],
        limits,
    ) {
        Err(StoreError::MachineAtLimit { active }) => assert_eq!(active, expected),
        other => panic!("expected the machine limit, got {other:?}"),
    }
    assert!(store.load("run-d").is_err(), "nothing was recorded");
    store
        .start_group_run_within(
            &record("third-33333333", "run-d", "D"),
            &["D".to_string()],
            RunLimits {
                project: None,
                machine: Some(3),
            },
        )
        .unwrap();
    store
        .start_group_run_within(
            &record("third-33333333", "run-e", "E"),
            &["E".to_string()],
            RunLimits::default(),
        )
        .unwrap();
}

#[test]
fn of_two_starts_at_once_in_two_projects_on_a_machine_allowing_one_exactly_one_passes() {
    for attempt in 0..10 {
        let store = std::sync::Arc::new(store(&format!("race-{attempt}")));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes: Vec<_> = ["p-11111111", "q-22222222"]
            .into_iter()
            .map(|project| {
                let (store, barrier) = (store.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let run = record(project, &format!("run-{project}"), "A");
                    barrier.wait();
                    store.start_group_run_within(
                        &run,
                        &["A".to_string()],
                        RunLimits {
                            project: None,
                            machine: Some(1),
                        },
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "{outcomes:?}"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, Err(StoreError::MachineAtLimit { .. }))),
            "{outcomes:?}"
        );
    }
}

#[test]
fn a_refusal_names_the_machine_its_active_runs_by_project_and_the_setting_to_raise() {
    let one = vec![pair("orq-cli-1a2b3c4d", "run-a")];
    assert_eq!(
        machine_limit_refusal(Some(1), &one).as_deref(),
        Some("this machine already has 1 active run (orq-cli run-a): wait for it to finish, or raise [runs] max_active"),
    );
    let two = vec![
        pair("orq-cli-1a2b3c4d", "run-a"),
        pair("giftcard-display-0f0f0f0f", "run-b"),
    ];
    assert_eq!(
        machine_limit_refusal(Some(2), &two).as_deref(),
        Some("this machine already has 2 active runs (orq-cli run-a, giftcard-display run-b): wait for one to finish, or raise [runs] max_active"),
    );
    assert_eq!(
        machine_limit_refusal(Some(3), &two),
        None,
        "under the limit"
    );
    assert_eq!(machine_limit_refusal(None, &two), None, "no limit");

    let free = |bytes: u64| FreeDisk {
        bytes,
        mount: std::path::PathBuf::from("/"),
    };
    assert_eq!(
        machine_disk_refusal(Some(15), &free(9_800_000_000)).as_deref(),
        Some("only 9.8 GB free on / (this machine needs 15 GB to start a run): free some space, or lower [runs] min_free_disk_gb"),
    );
    assert_eq!(
        machine_disk_refusal(Some(15), &free(15_000_000_000)),
        None,
        "exactly the floor is enough"
    );
    assert_eq!(machine_disk_refusal(None, &free(0)), None, "no floor");
}
