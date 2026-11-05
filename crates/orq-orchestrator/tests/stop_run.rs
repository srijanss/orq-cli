use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{owner_state_in, stop_run, Event, OwnerState, RunPhase, RunRecord, RunStore};

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

fn seeded(tag: &str, launched: Option<serde_json::Value>, extra: &[(&str, serde_json::Value)]) -> RunStore {
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
    for (kind, payload) in extra {
        store.append_event(&event(kind, payload.clone())).unwrap();
    }
    store
}

fn owner() -> serde_json::Value {
    serde_json::json!({ "pid": 4242, "start_ticks": 7 })
}

fn phase(store: &RunStore) -> RunPhase {
    orq_orchestrator::summarize_run(&store.events("run-1").unwrap()).phase
}

fn no_wait(_: std::time::Duration) {}

fn running(_: u32, _: u64) -> OwnerState {
    OwnerState::Running
}

fn proc_root(tag: &str) -> PathBuf {
    let root = orq_test_support::process_temp_dir().join(format!("orq-stop-proc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("self")).unwrap();
    root
}

fn put_stat(root: &std::path::Path, pid: u32, state: &str, start: u64) {
    std::fs::create_dir_all(root.join(pid.to_string())).unwrap();
    let filler = "1 ".repeat(18);
    std::fs::write(
        root.join(pid.to_string()).join("stat"),
        format!("{pid} (sl eep) {state} {filler}{start} 0 0\n"),
    )
    .unwrap();
}

#[test]
fn a_pid_that_cannot_be_a_single_process_is_never_signalled() {
    for pid in [0u64, 1, 1 << 32, (1 << 31) + 5] {
        let store = seeded("badpid", Some(serde_json::json!({ "pid": pid, "start_ticks": 7 })), &[]);
        let signals = RefCell::new(Vec::new());
        let result = stop_run(&store, "run-1", &running, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
        assert!(result.is_err(), "{pid}: {result:?}");
        assert!(signals.borrow().is_empty(), "{pid}");
        assert_eq!(phase(&store), RunPhase::Running);
    }
}

#[test]
fn an_owner_that_cannot_be_identified_is_not_signalled() {
    for (tag, launched, probe) in [
        ("noticks", serde_json::json!({ "pid": 4242 }), OwnerState::Running),
        ("unknown", owner(), OwnerState::Unknown),
    ] {
        let store = seeded(tag, Some(launched), &[]);
        let signals = RefCell::new(Vec::new());
        let result = stop_run(&store, "run-1", &|_, _| probe, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
        assert!(result.is_err(), "{tag}: {result:?}");
        assert!(signals.borrow().is_empty(), "{tag}");
        assert_eq!(phase(&store), RunPhase::Running, "{tag}");
    }
}

#[test]
fn an_owner_that_exits_after_the_term_signal_is_not_killed_and_the_run_is_recorded_failed() {
    let store = seeded("exits", Some(owner()), &[("run.pause_requested", serde_json::json!({}))]);
    assert_eq!(phase(&store), RunPhase::PauseRequested);
    let signals = RefCell::new(Vec::new());
    let polls = Cell::new(0);
    let probe = |_: u32, start: u64| {
        assert_eq!(start, 7);
        polls.set(polls.get() + 1);
        if polls.get() < 3 { OwnerState::Running } else { OwnerState::Exited }
    };
    let result = stop_run(&store, "run-1", &probe, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
    assert_eq!(result, Ok(()));
    assert_eq!(*signals.borrow(), vec![(4242, false)]);
    assert_eq!(phase(&store), RunPhase::Failed);
}

#[test]
fn an_owner_that_ignores_the_term_signal_is_killed_and_one_that_survives_that_keeps_the_run_running() {
    let store = seeded("ignores", Some(owner()), &[]);
    let signals = RefCell::new(Vec::new());
    let killed = Cell::new(false);
    let probe = |_: u32, _: u64| if killed.get() { OwnerState::Exited } else { OwnerState::Running };
    let result = stop_run(&store, "run-1", &probe, &|pid, force| { signals.borrow_mut().push((pid, force)); killed.set(force); Ok(()) }, &no_wait);
    assert_eq!(result, Ok(()));
    assert_eq!(*signals.borrow(), vec![(4242, false), (4242, true)]);
    assert_eq!(phase(&store), RunPhase::Failed);

    let store = seeded("survives", Some(owner()), &[]);
    let result = stop_run(&store, "run-1", &running, &|_, _| Ok(()), &no_wait);
    assert!(result.unwrap_err().contains("still running"));
    assert_eq!(phase(&store), RunPhase::Running);
}

#[test]
fn a_signal_error_is_reported_and_records_nothing() {
    let store = seeded("sigerr", Some(owner()), &[]);
    let result = stop_run(&store, "run-1", &running, &|_, _| Err("not permitted".into()), &no_wait);
    assert!(result.unwrap_err().contains("not permitted"));
    assert_eq!(phase(&store), RunPhase::Running);
}

#[test]
fn a_gone_or_absent_owner_is_not_signalled_and_the_run_is_recorded_failed() {
    for (tag, launched) in [("gone", Some(owner())), ("absent", None)] {
        let store = seeded(tag, launched, &[]);
        let signals = RefCell::new(Vec::new());
        let result = stop_run(&store, "run-1", &|_, _| OwnerState::Exited, &|pid, force| { signals.borrow_mut().push((pid, force)); Ok(()) }, &no_wait);
        assert_eq!(result, Ok(()), "{tag}");
        assert!(signals.borrow().is_empty(), "{tag}");
        assert_eq!(phase(&store), RunPhase::Failed, "{tag}");
    }
}

#[test]
fn a_run_that_is_not_running_is_refused_and_left_untouched() {
    let ended = serde_json::json!({ "ending": "review_ready" });
    let histories: [(&str, Vec<(&str, serde_json::Value)>); 4] = [
        ("failed", vec![("run.failed", serde_json::json!({}))]),
        ("completed", vec![("run.completed", ended)]),
        ("paused", vec![("run.pause_requested", serde_json::json!({})), ("run.paused", serde_json::json!({}))]),
        ("blocked", vec![("run.blocked", serde_json::json!({}))]),
    ];
    for (tag, extra) in histories {
        let store = seeded(tag, Some(owner()), &extra);
        let before = store.events("run-1").unwrap().len();
        let signals = Cell::new(0);
        let result = stop_run(&store, "run-1", &running, &|_, _| { signals.set(signals.get() + 1); Ok(()) }, &no_wait);
        assert!(result.unwrap_err().contains("only a running run"), "{tag}");
        assert_eq!(signals.get(), 0, "{tag}");
        assert_eq!(store.events("run-1").unwrap().len(), before, "{tag}");
    }
}

#[test]
fn the_owner_probe_tells_a_running_process_from_a_gone_zombie_or_reused_one_and_fails_closed() {
    let root = proc_root("probe");
    put_stat(&root, 4242, "S", 7);
    assert_eq!(owner_state_in(&root, 4242, 7), OwnerState::Running);
    assert_eq!(owner_state_in(&root, 4242, 8), OwnerState::Exited, "pid reused by a newer process");
    put_stat(&root, 4243, "Z", 7);
    assert_eq!(owner_state_in(&root, 4243, 7), OwnerState::Exited, "an unreaped zombie has exited");
    assert_eq!(owner_state_in(&root, 4244, 7), OwnerState::Exited, "no such process");

    std::fs::create_dir_all(root.join("4245")).unwrap();
    std::fs::write(root.join("4245").join("stat"), "garbage").unwrap();
    assert_eq!(owner_state_in(&root, 4245, 7), OwnerState::Unknown, "unparsable identity");

    let no_proc = orq_test_support::process_temp_dir().join(format!("orq-stop-noproc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&no_proc);
    std::fs::create_dir_all(&no_proc).unwrap();
    assert_eq!(owner_state_in(&no_proc, 4242, 7), OwnerState::Unknown, "no procfs to verify against");
}
