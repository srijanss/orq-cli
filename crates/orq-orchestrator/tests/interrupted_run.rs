use std::collections::BTreeMap;

use orq_orchestrator::{reconcile_interrupted, record_launch, summarize_run, RunPhase, RunRecord, RunStore};

fn record(id: &str) -> RunRecord {
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
        profiles: BTreeMap::new(),
    }
}

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-interrupted-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    RunStore::new(&dir)
}

#[test]
fn a_running_run_whose_owner_is_dead_is_marked_failed_so_it_can_be_resumed() {
    let store = store("dead");
    store.create(&record("run-1")).unwrap();
    record_launch(&store, "run-1", 4242, std::path::Path::new("/tmp/x.log")).unwrap();
    assert_eq!(summarize_run(&store.events("run-1").unwrap()).phase, RunPhase::Running);

    let changed = reconcile_interrupted(&store, "run-1", &|_pid| false).unwrap();

    assert!(changed);
    let summary = summarize_run(&store.events("run-1").unwrap());
    assert_eq!(summary.phase, RunPhase::Failed);
    assert!(summary.reason.unwrap().contains("4242"));
}
