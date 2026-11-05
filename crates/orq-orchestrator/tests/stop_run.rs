use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{stop_run, Event, RunPhase, RunRecord, RunStore};

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}-{}", payload),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

fn seeded(tag: &str, launched: Option<serde_json::Value>, extra: &[&str]) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!("orq-stop-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-1".into(),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store.append_event(&event("stage.started", serde_json::json!({}))).unwrap();
    if let Some(payload) = launched {
        store.append_event(&event("run.launched", payload)).unwrap();
    }
    for kind in extra {
        store.append_event(&event(kind, serde_json::json!({}))).unwrap();
    }
    store
}

fn phase(store: &RunStore) -> RunPhase {
    orq_orchestrator::summarize_run(&store.events("run-1").unwrap()).phase
}

fn no_wait(_: std::time::Duration) {}

#[test]
fn a_pid_that_cannot_be_a_single_process_is_never_signalled() {
    for pid in [0u64, 1 << 32, (1 << 31) + 5] {
        let store = seeded("badpid", Some(serde_json::json!({ "pid": pid, "start_ticks": 7 })), &[]);
        let signals = RefCell::new(Vec::new());
        let result = stop_run(&store, "run-1", &|_, _| true, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
        assert!(result.is_err(), "{pid}: {result:?}");
        assert!(signals.borrow().is_empty(), "{pid}");
        assert_eq!(phase(&store), RunPhase::Running);
    }
}

#[test]
fn an_owner_whose_identity_cannot_be_verified_is_not_signalled() {
    let store = seeded("noticks", Some(serde_json::json!({ "pid": 4242 })), &[]);
    let signals = RefCell::new(Vec::new());
    let result = stop_run(&store, "run-1", &|_, _| true, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
    assert!(result.is_err(), "{result:?}");
    assert!(signals.borrow().is_empty());
    assert_eq!(phase(&store), RunPhase::Running);
}

#[test]
fn an_owner_that_exits_after_the_term_signal_is_not_killed_and_the_run_is_recorded_failed() {
    let store = seeded("exits", Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &["run.pause_requested"]);
    let signals = RefCell::new(Vec::new());
    let polls = Cell::new(0);
    let alive = |_: u32, start: Option<u64>| {
        assert_eq!(start, Some(7));
        polls.set(polls.get() + 1);
        polls.get() < 3
    };
    let result = stop_run(&store, "run-1", &alive, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
    assert_eq!(result, Ok(()));
    assert_eq!(*signals.borrow(), vec![(4242, false)]);
    assert_eq!(phase(&store), RunPhase::Failed);
}

#[test]
fn an_owner_that_ignores_the_term_signal_is_killed_and_one_that_survives_that_keeps_the_run_running() {
    let store = seeded("ignores", Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &[]);
    let signals = RefCell::new(Vec::new());
    let killed = Cell::new(false);
    let alive = |_: u32, _: Option<u64>| !killed.get();
    let result = stop_run(&store, "run-1", &alive, &|pid, force| { signals.borrow_mut().push((pid, force)); killed.set(force); Ok(()) }, &no_wait);
    assert_eq!(result, Ok(()));
    assert_eq!(*signals.borrow(), vec![(4242, false), (4242, true)]);
    assert_eq!(phase(&store), RunPhase::Failed);

    let store = seeded("survives", Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &[]);
    let result = stop_run(&store, "run-1", &|_, _| true, &|_, _| Ok(()), &no_wait);
    assert!(result.unwrap_err().contains("still running"));
    assert_eq!(phase(&store), RunPhase::Running);
}

#[test]
fn a_signal_error_is_reported_and_records_nothing() {
    let store = seeded("sigerr", Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &[]);
    let result = stop_run(&store, "run-1", &|_, _| true, &|_, _| Err("not permitted".into()), &no_wait);
    assert!(result.unwrap_err().contains("not permitted"));
    assert_eq!(phase(&store), RunPhase::Running);
}

#[test]
fn a_gone_owner_is_not_signalled_and_a_finished_or_paused_run_is_refused() {
    let store = seeded("gone", Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &[]);
    let signals = RefCell::new(Vec::new());
    let result = stop_run(&store, "run-1", &|_, _| false, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
    assert_eq!(result, Ok(()));
    assert!(signals.borrow().is_empty());
    assert_eq!(phase(&store), RunPhase::Failed);

    for kind in ["run.failed", "worktree.removed"] {
        let store = seeded(kind, Some(serde_json::json!({ "pid": 4242, "start_ticks": 7 })), &[kind]);
        let before = store.events("run-1").unwrap().len();
        let result = stop_run(&store, "run-1", &|_, _| true, &|_, _| Ok(()), &no_wait);
        assert!(result.unwrap_err().contains("only a running run"));
        assert_eq!(store.events("run-1").unwrap().len(), before);
    }
}
