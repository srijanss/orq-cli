//! The ledger features a run's fix sessions started are recorded in the run's events, so the run
//! can close them when it ends and a resumed run can carry them on.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{carry_fix_features, features_no_later_run_took, fix_features, record_fix_features_added, RunRecord, RunStore};

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
