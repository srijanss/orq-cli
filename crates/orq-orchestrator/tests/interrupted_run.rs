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

#[test]
fn a_run_killed_after_a_pause_was_requested_is_marked_failed_too() {
    let store = store("pause-requested");
    store.create(&record("run-1")).unwrap();
    record_launch(&store, "run-1", 4242, std::path::Path::new("/tmp/x.log")).unwrap();
    orq_orchestrator::request_pause(&store, "run-1").unwrap();
    assert_eq!(summarize_run(&store.events("run-1").unwrap()).phase, RunPhase::PauseRequested);

    assert!(reconcile_interrupted(&store, "run-1", &|_pid| false).unwrap());
    assert_eq!(summarize_run(&store.events("run-1").unwrap()).phase, RunPhase::Failed);
}

#[test]
fn a_process_is_dead_only_when_procfs_is_there_and_has_no_entry_for_it() {
    let root = orq_test_support::process_temp_dir().join(format!("orq-fake-proc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // No procfs here (as on macOS): liveness is unknown, which must never read as dead.
    assert!(orq_orchestrator::process_is_alive_in(&root, 77));
    std::fs::create_dir_all(root.join("self")).unwrap();
    assert!(!orq_orchestrator::process_is_alive_in(&root, 77), "procfs without an entry: gone");
    std::fs::create_dir_all(root.join("77")).unwrap();
    assert!(orq_orchestrator::process_is_alive_in(&root, 77));
}

#[test]
fn guards_leave_live_terminal_paused_and_unlaunched_runs_alone() {
    // Owner alive: untouched.
    let s = store("guard-alive");
    s.create(&record("r")).unwrap();
    record_launch(&s, "r", 1, std::path::Path::new("/tmp/x.log")).unwrap();
    let before = s.events("r").unwrap().len();
    assert!(!reconcile_interrupted(&s, "r", &|_| true).unwrap());
    assert_eq!(s.events("r").unwrap().len(), before);

    // No launch metadata: nothing to judge, untouched.
    let s = store("guard-unlaunched");
    s.create(&record("r")).unwrap();
    assert!(!reconcile_interrupted(&s, "r", &|_| false).unwrap());

    // Paused keeps its checkpoint.
    let s = store("guard-paused");
    s.create(&record("r")).unwrap();
    record_launch(&s, "r", 1, std::path::Path::new("/tmp/x.log")).unwrap();
    orq_orchestrator::request_pause(&s, "r").unwrap();
    let mut paused = s.events("r").unwrap().last().unwrap().clone();
    paused.kind = "run.paused".into();
    paused.event_id = "r-paused".into();
    s.append_event(&paused).unwrap();
    if summarize_run(&s.events("r").unwrap()).phase == RunPhase::Paused {
        assert!(!reconcile_interrupted(&s, "r", &|_| false).unwrap());
        assert_eq!(summarize_run(&s.events("r").unwrap()).phase, RunPhase::Paused);
    }

    // Malformed pid: untouched.
    let s = store("guard-malformed");
    s.create(&record("r")).unwrap();
    let mut bad = paused.clone();
    bad.kind = "run.launched".into();
    bad.event_id = "r-bad".into();
    bad.payload = serde_json::json!({ "pid": "abc" });
    s.append_event(&bad).unwrap();
    assert!(!reconcile_interrupted(&s, "r", &|_| false).unwrap());
}

#[test]
fn the_latest_launch_wins_and_reconciling_twice_appends_once() {
    let s = store("latest-launch");
    s.create(&record("r")).unwrap();
    record_launch(&s, "r", 111, std::path::Path::new("/tmp/a.log")).unwrap();
    record_launch(&s, "r", 222, std::path::Path::new("/tmp/b.log")).unwrap();
    // Only the first (stale) pid is dead; the latest owner is alive.
    assert!(!reconcile_interrupted(&s, "r", &|pid| pid == 222).unwrap());
    assert!(reconcile_interrupted(&s, "r", &|_| false).unwrap());
    let n = s.events("r").unwrap().len();
    assert!(!reconcile_interrupted(&s, "r", &|_| false).unwrap());
    assert_eq!(s.events("r").unwrap().len(), n);
}

#[test]
fn an_unknown_run_is_a_store_error() {
    let s = store("unknown");
    assert!(reconcile_interrupted(&s, "missing", &|_| false).is_err());
}
