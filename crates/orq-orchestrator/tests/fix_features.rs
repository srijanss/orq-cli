//! The ledger features a run's fix sessions started are recorded in the run's events, so the run
//! can close them when it ends and a resumed run can carry them on.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{carry_fix_features, carry_session_references, features_no_later_run_took, fix_features, record_fix_features_added, Event, RunRecord, RunStore};

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-fix-features-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    RunStore::new(&dir)
}

fn record(run_id: &str) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "feat".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--feat".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--feat"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

#[test]
fn the_features_every_fix_session_added_are_read_back_once_each_in_order() {
    let store = store("recorded");
    store.start_run(&record("run-1")).unwrap();

    record_fix_features_added(&store, &record("run-1"), "gate_fix", &names(&["b", "a"])).unwrap();
    record_fix_features_added(&store, &record("run-1"), "ci_fix", &names(&["a", "c"])).unwrap();

    assert_eq!(fix_features(&store, "run-1").unwrap(), names(&["b", "a", "c"]));
    let events = store.events("run-1").unwrap();
    let added: Vec<_> = events.iter().filter(|e| e.kind == "fix.features_added").map(|e| e.payload.clone()).collect();
    assert_eq!(added[0], serde_json::json!({ "session": "gate_fix", "features": ["b", "a"] }));
}

#[test]
fn a_resumed_run_carries_the_features_of_the_run_it_resumes() {
    let store = store("carried");
    store.start_run(&record("run-1")).unwrap();
    record_fix_features_added(&store, &record("run-1"), "gate_fix", &names(&["a"])).unwrap();
    store.finish_run("repo-1", "feat", "run-1").unwrap();
    store.start_run(&record("run-2")).unwrap();

    carry_fix_features(&store, &record("run-2"), "run-1").unwrap();

    assert_eq!(fix_features(&store, "run-2").unwrap(), names(&["a"]));
}

#[test]
fn a_feature_a_later_run_carried_on_from_a_fix_session_is_not_free_for_the_earlier_run_to_close() {
    let store = store("carried-taken");
    store.start_run(&record("run-1")).unwrap();
    record_fix_features_added(&store, &record("run-1"), "gate_fix", &names(&["a", "b"])).unwrap();
    store.finish_run("repo-1", "feat", "run-1").unwrap();
    store.start_run(&record("run-2")).unwrap();
    record_fix_features_added(&store, &record("run-2"), "carried", &names(&["a"])).unwrap();

    let free = features_no_later_run_took(&store, "run-1", &names(&["a", "b"])).unwrap();

    assert_eq!(free, names(&["b"]), "run-2 took a on; b is still run-1's alone");
}

/// The run a run resumed is never a later run of its features, however their starts compare, so
/// two runs started in the same second are covered too: what the resumed run's fix sessions started
/// is the resuming run's to close. The resuming run is launched first, at a fixed time in the past,
/// so the resumed run never started earlier than it whatever the clock reads: a run's start is its
/// first event, and `start_run` records none.
#[test]
fn the_run_a_run_resumed_never_counts_as_a_later_run_that_took_its_carried_features() {
    let store = store("resumed-not-earlier");
    let resuming = RunRecord { feature_id: "feat-2".to_string(), ..record("run-2") };
    store.start_run(&resuming).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "run-2-launched".to_string(),
            run_id: "run-2".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "feat-2".to_string(),
            kind: "run.launched".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    store.start_run(&record("run-1")).unwrap();
    record_fix_features_added(&store, &record("run-1"), "gate_fix", &names(&["a"])).unwrap();
    store.finish_run("repo-1", "feat", "run-1").unwrap();
    carry_fix_features(&store, &resuming, "run-1").unwrap();
    let started = |run: &str| store.events(run).unwrap().first().unwrap().occurred_at.clone();
    assert!(started("run-1") >= started("run-2"), "run-1 did not start earlier than run-2");

    let free = features_no_later_run_took(&store, "run-2", &names(&["a"])).unwrap();

    assert_eq!(free, names(&["a"]), "run-1 is the run run-2 resumed, not a later one");
}

/// A resumed run whose fix sessions started nothing still carries on from the run it resumed: that
/// run's own feature is the resuming run's, though both started in the same second.
#[test]
fn the_run_a_run_resumed_never_counts_as_a_later_run_that_took_its_feature() {
    let store = store("resumed-own-feature");
    // run-1 records no events, so it has no start time and could only count as later.
    store.start_run(&record("run-1")).unwrap();
    store.finish_run("repo-1", "feat", "run-1").unwrap();
    store.start_run(&record("run-2")).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "run-2-launched".to_string(),
            run_id: "run-2".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "feat".to_string(),
            kind: "run.launched".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    carry_session_references(&store, &record("run-2"), "run-1").unwrap();

    let free = features_no_later_run_took(&store, "run-2", &names(&["feat"])).unwrap();

    assert_eq!(free, names(&["feat"]), "run-1 is the run run-2 resumed, not a later one");
}

/// A run resumed a run that had resumed another: run-2 resumed run-1 before its fix sessions started
/// anything, and run-3 carried run-2's fix features and sessions on. run-1 is an ancestor of run-3
/// whichever way run-3 reached run-2, so it is never a later run that took run-3's feature.
#[test]
fn a_run_resumed_by_the_run_a_run_carried_fix_features_from_is_an_ancestor_too() {
    let store = store("mixed-ancestry");
    // run-1 records no events, so it has no start time and could only count as later.
    store.start_run(&record("run-1")).unwrap();
    store.finish_run("repo-1", "feat", "run-1").unwrap();
    store.start_run(&record("run-2")).unwrap();
    carry_session_references(&store, &record("run-2"), "run-1").unwrap();
    record_fix_features_added(&store, &record("run-2"), "gate_fix", &names(&["x"])).unwrap();
    store.finish_run("repo-1", "feat", "run-2").unwrap();
    store.start_run(&record("run-3")).unwrap();
    carry_fix_features(&store, &record("run-3"), "run-2").unwrap();
    carry_session_references(&store, &record("run-3"), "run-2").unwrap();

    let free = features_no_later_run_took(&store, "run-3", &names(&["feat", "x"])).unwrap();

    assert_eq!(free, names(&["feat", "x"]), "run-1 and run-2 are the runs run-3 resumed, not later ones");
}
