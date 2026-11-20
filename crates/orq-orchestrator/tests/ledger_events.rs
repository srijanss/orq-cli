//! What a run did to the ledger on the agent's behalf (handed a feature back, completed it at
//! delivery, saw the session add another) is recorded in the run's events.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{record_ledger_features, RunRecord, RunStore};

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-ledger-events-{}-{tag}", std::process::id()));
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

#[test]
fn the_features_a_ledger_change_touched_are_recorded_under_its_kind() {
    let store = store("recorded");
    store.start_run(&record("run-1")).unwrap();

    record_ledger_features(
        &store,
        &record("run-1"),
        "ledger.handed_back",
        &["a".to_string(), "b".to_string()],
    )
    .unwrap();

    let events = store.events("run-1").unwrap();
    let event = events
        .iter()
        .find(|e| e.kind == "ledger.handed_back")
        .expect("a ledger.handed_back event");
    assert_eq!(event.payload, serde_json::json!({ "features": ["a", "b"] }));
    assert_eq!(
        (event.run_id.as_str(), event.feature_id.as_str()),
        ("run-1", "feat")
    );
}
