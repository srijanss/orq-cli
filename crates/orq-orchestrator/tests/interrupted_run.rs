use std::collections::BTreeMap;

use orq_orchestrator::{
    reconcile_interrupted, record_launch, summarize_run, RunPhase, RunRecord, RunStore,
};

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
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-interrupted-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    RunStore::new(&dir)
}

#[test]
fn a_running_run_whose_owner_is_dead_is_marked_failed_so_it_can_be_resumed() {
    let store = store("dead");
    store.create(&record("run-1")).unwrap();
    record_launch(&store, "run-1", 4242, std::path::Path::new("/tmp/x.log")).unwrap();
    assert_eq!(
        summarize_run(&store.events("run-1").unwrap()).phase,
        RunPhase::Running
    );

    let changed = reconcile_interrupted(&store, "run-1", &|_pid, _| false).unwrap();

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
    assert_eq!(
        summarize_run(&store.events("run-1").unwrap()).phase,
        RunPhase::PauseRequested
    );

    assert!(reconcile_interrupted(&store, "run-1", &|_pid, _| false).unwrap());
    assert_eq!(
        summarize_run(&store.events("run-1").unwrap()).phase,
        RunPhase::Failed
    );
}

#[test]
fn a_process_is_dead_only_when_procfs_is_there_and_has_no_entry_for_it() {
    let root =
        orq_test_support::process_temp_dir().join(format!("orq-fake-proc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // No procfs here (as on macOS): liveness is unknown, which must never read as dead.
    assert!(orq_orchestrator::process_is_alive_in(&root, 77));
    std::fs::create_dir_all(root.join("self")).unwrap();
    assert!(
        !orq_orchestrator::process_is_alive_in(&root, 77),
        "procfs without an entry: gone"
    );
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
    assert!(!reconcile_interrupted(&s, "r", &|_, _| true).unwrap());
    assert_eq!(s.events("r").unwrap().len(), before);

    // No launch metadata: nothing to judge, untouched.
    let s = store("guard-unlaunched");
    s.create(&record("r")).unwrap();
    assert!(!reconcile_interrupted(&s, "r", &|_, _| false).unwrap());

    // Paused keeps its checkpoint.
    let s = store("guard-paused");
    s.create(&record("r")).unwrap();
    record_launch(&s, "r", 1, std::path::Path::new("/tmp/x.log")).unwrap();
    orq_orchestrator::request_pause(&s, "r").unwrap();
    let mut paused = s.events("r").unwrap().last().unwrap().clone();
    paused.kind = "run.paused".into();
    paused.event_id = "r-paused".into();
    s.append_event(&paused).unwrap();
    assert_eq!(summarize_run(&s.events("r").unwrap()).phase, RunPhase::Paused, "precondition");
    let n = s.events("r").unwrap().len();
    assert!(!reconcile_interrupted(&s, "r", &|_, _| false).unwrap());
    assert_eq!(s.events("r").unwrap().len(), n);
    assert_eq!(summarize_run(&s.events("r").unwrap()).phase, RunPhase::Paused);

    // Ended runs (completed, blocked) keep their ending even with a dead owner on record.
    for (tag, ending, phase) in [("completed", "review_ready", RunPhase::Completed), ("blocked", "blocked", RunPhase::Blocked)] {
        let s = store(&format!("guard-{tag}"));
        let rec = record("r");
        s.create(&rec).unwrap();
        record_launch(&s, "r", 1, std::path::Path::new("/tmp/x.log")).unwrap();
        orq_orchestrator::record_run_completed(&s, &rec, ending, None).unwrap();
        assert_eq!(summarize_run(&s.events("r").unwrap()).phase, phase, "precondition");
        let n = s.events("r").unwrap().len();
        assert!(!reconcile_interrupted(&s, "r", &|_, _| false).unwrap(), "{tag}");
        assert_eq!(s.events("r").unwrap().len(), n, "{tag}");
        assert_eq!(summarize_run(&s.events("r").unwrap()).phase, phase, "{tag}");
    }

    // Malformed pid: untouched.
    let s = store("guard-malformed");
    s.create(&record("r")).unwrap();
    let mut bad = paused.clone();
    bad.kind = "run.launched".into();
    bad.event_id = "r-bad".into();
    bad.payload = serde_json::json!({ "pid": "abc" });
    s.append_event(&bad).unwrap();
    assert!(!reconcile_interrupted(&s, "r", &|_, _| false).unwrap());
}

#[test]
fn the_latest_launch_wins_and_reconciling_twice_appends_once() {
    let s = store("latest-launch");
    s.create(&record("r")).unwrap();
    record_launch(&s, "r", 111, std::path::Path::new("/tmp/a.log")).unwrap();
    record_launch(&s, "r", 222, std::path::Path::new("/tmp/b.log")).unwrap();
    // Only the first (stale) pid is dead; the latest owner is alive.
    assert!(!reconcile_interrupted(&s, "r", &|pid, _| pid == 222).unwrap());
    assert!(reconcile_interrupted(&s, "r", &|_, _| false).unwrap());
    let n = s.events("r").unwrap().len();
    assert!(!reconcile_interrupted(&s, "r", &|_, _| false).unwrap());
    assert_eq!(s.events("r").unwrap().len(), n);
}

#[test]
fn an_unknown_run_is_a_store_error() {
    let s = store("unknown");
    assert!(reconcile_interrupted(&s, "missing", &|_, _| false).is_err());
}

fn fake_stat(root: &std::path::Path, pid: u32, start_ticks: u64) {
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    // `comm` may hold spaces and parentheses; starttime is field 22 overall, so index 19 after the last ')'.
    let mut fields: Vec<String> = (0..19).map(|i| i.to_string()).collect();
    fields[0] = "S".into();
    fields.push(start_ticks.to_string());
    std::fs::write(
        dir.join("stat"),
        format!("{pid} (we ird) name) {}\n", fields.join(" ")),
    )
    .unwrap();
}

#[test]
fn a_pid_reused_by_another_process_is_not_the_recorded_owner() {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-fake-proc-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("self")).unwrap();
    fake_stat(&root, 77, 200);
    assert_eq!(
        orq_orchestrator::process_start_ticks_in(&root, 77),
        Some(200)
    );
    assert_eq!(orq_orchestrator::process_start_ticks_in(&root, 78), None);

    assert!(
        orq_orchestrator::owner_is_alive_in(&root, 77, Some(200)),
        "same start time: the owner"
    );
    assert!(
        !orq_orchestrator::owner_is_alive_in(&root, 77, Some(100)),
        "different start time: pid reused"
    );
    assert!(
        orq_orchestrator::owner_is_alive_in(&root, 77, None),
        "legacy pid-only record stays conservative"
    );
}

#[test]
fn a_launch_records_the_owners_start_time_and_reconciliation_hands_it_to_the_check() {
    let s = store("start-ticks");
    s.create(&record("r")).unwrap();
    record_launch(
        &s,
        "r",
        std::process::id(),
        std::path::Path::new("/tmp/x.log"),
    )
    .unwrap();
    let ticks =
        orq_orchestrator::process_start_ticks_in(std::path::Path::new("/proc"), std::process::id());
    let launched = s
        .events("r")
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.kind == "run.launched")
        .unwrap();
    assert_eq!(
        launched.payload.get("start_ticks").and_then(|t| t.as_u64()),
        ticks
    );

    let seen = std::cell::Cell::new(None);
    reconcile_interrupted(&s, "r", &|_pid, recorded| {
        seen.set(Some(recorded));
        true
    })
    .unwrap();
    assert_eq!(seen.get(), Some(ticks));
}

#[test]
fn planning_a_resume_of_an_interrupted_run_frees_the_feature_it_still_held() {
    let s = store("frees-lock");
    let old = record("run-1");
    let members = vec!["F".to_string()];
    s.start_group_run(&old, &members).unwrap();
    record_launch(&s, "run-1", 4242, std::path::Path::new("/tmp/x.log")).unwrap();

    // The plan itself may be refused (there is no checkout here); the dead run's lock must be gone anyway.
    let _ = orq_orchestrator::plan_group_resume_with(
        &s,
        std::path::Path::new("/tmp/none"),
        "p",
        &members,
        &|_, _| false,
    );

    s.start_group_run(&record("run-2"), &members)
        .expect("a new run can take the feature");
}

fn lock_owner(s: &RunStore, feature: &str) -> Option<String> {
    s.active_run("p", feature).unwrap()
}

#[test]
fn releasing_an_interrupted_runs_locks_is_retryable_and_only_ever_its_own() {
    let s = store("group-release");
    let members = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    let mut old = record("run-1");
    old.feature_id = "A".into();
    s.start_group_run(&old, &members).unwrap();
    record_launch(&s, "run-1", 4242, std::path::Path::new("/tmp/x.log")).unwrap();
    orq_orchestrator::record_group(&s, &old, &members).unwrap();

    // Reconciliation already persisted run.failed (an earlier attempt died before releasing).
    assert!(reconcile_interrupted(&s, "run-1", &|_, _| false).unwrap());
    // B's lock is already gone; C was taken over by a newer run.
    s.finish_run("p", "B", "run-1").unwrap();
    s.finish_run("p", "C", "run-1").unwrap();
    let mut newer = record("run-2");
    newer.feature_id = "C".into();
    s.start_group_run(&newer, &["C".to_string()]).unwrap();

    let _ = orq_orchestrator::plan_group_resume_with(
        &s,
        std::path::Path::new("/tmp/none"),
        "p",
        &members,
        &|_, _| false,
    );

    assert_eq!(lock_owner(&s, "A"), None, "the remaining lock is freed on retry");
    assert_eq!(lock_owner(&s, "C").as_deref(), Some("run-2"), "a newer owner is kept");
}
