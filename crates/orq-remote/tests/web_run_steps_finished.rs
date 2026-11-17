//! A run that ended shows its last step, cleanup, as finished: the Timeline gives it a tick and the Events
//! tab a check, and both tabs give every step the same state. The event lists are copied from real runs.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const STATES: [&str; 4] = ["done", "failed", "current", "open"];

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// Events as (kind, stage, attempt, time), numbered in order.
fn events(list: &[(&str, Option<&str>, u32, &str)]) -> Value {
    list.iter().enumerate().map(|(at, (kind, stage, attempt, time))| json!({"event_id": format!("e{at}"), "kind": kind, "stage": stage, "attempt": attempt, "occurred_at": time})).collect()
}

fn selectors() -> Vec<String> {
    let mut all = vec![".run-steps > li:last-child[data-step=done]".to_string(), ".run-cleanup-note".to_string()];
    for state in STATES {
        all.push(format!(".run-step[data-step={state}]"));
        all.push(format!(".attempt[data-step={state}]"));
    }
    all.extend([".attempt .step-icon.step-done", ".attempt .step-icon.step-failed", ".attempt .step-icon.step-open"].map(String::from));
    all
}

fn open_run(state: &str, events: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": state, "stage": "cleanup", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": [page, projects()], "steps": [{"submit": TOKEN}], "selectors": selectors()});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"][1].clone())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

/// How many steps each tab gives each state: (Timeline, Events).
fn states(frame: &Value) -> Vec<(&'static str, u64, u64)> {
    STATES.iter().map(|state| (*state, count(frame, &format!(".run-step[data-step={state}]")), count(frame, &format!(".attempt[data-step={state}]")))).collect()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn assert_tabs_agree(frame: &Value) {
    for (state, timeline, tab) in states(frame) {
        assert_eq!(timeline, tab, "{state}: the Timeline and the Events tab disagree: {:?}", states(frame));
    }
}

// run-1794841549-922432-1, delivered locally (its first, failed verify left out).
fn delivered_locally() -> Value {
    events(&[
        ("run.launched", None, 1, "2026-11-16T15:05:49Z"),
        ("stage.started", Some("implement_and_review"), 1, "2026-11-16T15:05:51Z"),
        ("stage.completed", Some("implement_and_review"), 1, "2026-11-16T15:16:59Z"),
        ("session.notes", Some("implement_and_review"), 1, "2026-11-16T15:16:59Z"),
        ("rebase.completed", Some("rebase"), 1, "2026-11-16T15:17:03Z"),
        ("stage.started", Some("verify"), 2, "2026-11-16T15:27:21Z"),
        ("stage.completed", Some("verify"), 2, "2026-11-16T15:35:08Z"),
        ("merge.completed", Some("merge_local"), 1, "2026-11-16T15:35:08Z"),
        ("worktree.removed", Some("cleanup"), 1, "2026-11-16T15:35:11Z"),
        ("run.completed", None, 1, "2026-11-16T15:35:11Z"),
    ])
}

#[test]
fn a_run_delivered_locally_shows_every_step_done_in_both_tabs() {
    let Some(frame) = open_run("completed", delivered_locally()) else { return };
    assert_eq!(states(&frame), vec![("done", 5, 5), ("failed", 0, 0), ("current", 0, 0), ("open", 0, 0)], "{frame}");
    assert_eq!(count(&frame, ".run-steps > li:last-child[data-step=done]"), 1, "cleanup has its tick");
    assert_eq!(count(&frame, ".attempt .step-icon.step-done"), 5, "each step has a check in the Events tab");
    assert_eq!(count(&frame, ".attempt .step-icon.step-open"), 0, "no clock");
    assert!(!texts(&frame).iter().any(|t| t == "Not finished"), "{:?}", texts(&frame));
}

// run-1794793604-2222865-1, delivered by review: the worktree is kept, and deleted later once the review ends.
fn delivered_by_review(deleted: bool) -> Value {
    let mut list = vec![
        ("stage.started", Some("verify"), 1, "2026-11-16T08:20:00Z"),
        ("stage.completed", Some("verify"), 1, "2026-11-16T08:29:39Z"),
        ("stage.started", Some("push"), 1, "2026-11-16T08:29:39Z"),
        ("stage.completed", Some("push"), 1, "2026-11-16T08:29:43Z"),
        ("stage.started", Some("ci"), 1, "2026-11-16T08:29:43Z"),
        ("stage.completed", Some("ci"), 1, "2026-11-16T08:39:58Z"),
        ("worktree.kept", Some("cleanup"), 1, "2026-11-16T08:39:58Z"),
        ("run.completed", None, 1, "2026-11-16T08:39:58Z"),
    ];
    if deleted {
        list.push(("worktree.deleted", Some("cleanup"), 1, "2026-11-16T09:23:55Z"));
    }
    events(&list)
}

#[test]
fn a_run_delivered_by_review_shows_cleanup_done_with_the_worktree_kept_then_deleted() {
    for (deleted, note) in [(false, "Worktree kept"), (true, "Worktree deleted")] {
        let Some(frame) = open_run("needs_review", delivered_by_review(deleted)) else { return };
        assert_eq!(states(&frame), vec![("done", 4, 4), ("failed", 0, 0), ("current", 0, 0), ("open", 0, 0)], "{deleted}: {frame}");
        assert_eq!(count(&frame, ".run-steps > li:last-child[data-step=done]"), 1, "{deleted}");
        assert_eq!(count(&frame, ".run-cleanup-note"), 1, "{deleted}: {frame}");
        assert!(texts(&frame).iter().any(|t| t == note), "{note}: {:?}", texts(&frame));
        let other = if deleted { "Worktree kept" } else { "Worktree deleted" };
        assert!(!texts(&frame).iter().any(|t| t == other), "{other}: {:?}", texts(&frame));
    }
}

#[test]
fn a_failed_verify_stays_failed_in_both_tabs_though_the_run_completed() {
    let failed = events(&[
        ("stage.started", Some("implement_and_review"), 1, "2026-11-15T10:51:31Z"),
        ("stage.completed", Some("implement_and_review"), 1, "2026-11-15T11:19:14Z"),
        ("stage.started", Some("verify"), 1, "2026-11-15T11:19:19Z"),
        ("stage.failed", Some("verify"), 1, "2026-11-15T11:26:43Z"),
        ("run.failed", None, 1, "2026-11-15T11:26:43Z"),
        ("run.completed", None, 1, "2026-11-15T11:26:43Z"),
    ]);
    let Some(frame) = open_run("failed", failed) else { return };
    assert_eq!(states(&frame), vec![("done", 1, 1), ("failed", 1, 1), ("current", 0, 0), ("open", 0, 0)], "{frame}");
    assert_eq!(count(&frame, ".attempt .step-icon.step-failed"), 1);
}

fn under_way() -> Value {
    events(&[
        ("stage.started", Some("implement_and_review"), 1, "2026-11-16T15:05:51Z"),
        ("stage.completed", Some("implement_and_review"), 1, "2026-11-16T15:16:59Z"),
        ("stage.started", Some("verify"), 1, "2026-11-16T15:17:03Z"),
    ])
}

#[test]
fn a_running_runs_last_step_is_in_progress_and_a_paused_runs_is_not_finished() {
    let Some(running) = open_run("running", under_way()) else { return };
    assert_eq!(states(&running), vec![("done", 1, 1), ("failed", 0, 0), ("current", 1, 1), ("open", 0, 0)], "{running}");
    assert_eq!(count(&running, ".attempt .step-icon.step-open"), 1, "a clock on the step under way");
    let Some(paused) = open_run("paused", under_way()) else { return };
    assert_eq!(states(&paused), vec![("done", 1, 1), ("failed", 0, 0), ("current", 0, 0), ("open", 1, 1)], "{paused}");
    assert!(texts(&paused).iter().any(|t| t == "Not finished"), "{:?}", texts(&paused));
}

#[test]
fn a_step_only_started_with_no_later_step_stays_open_even_after_run_completed_on_a_stopped_run() {
    let mut list = under_way();
    list.as_array_mut().unwrap().push(json!({"event_id": "e9", "kind": "run.completed", "stage": null, "attempt": 1, "occurred_at": "2026-11-16T15:20:00Z"}));
    let Some(frame) = open_run("blocked", list) else { return };
    assert_eq!(states(&frame), vec![("done", 1, 1), ("failed", 0, 0), ("current", 0, 0), ("open", 1, 1)], "{frame}");
}

#[test]
fn the_two_tabs_give_every_step_the_same_state() {
    for (state, list) in [("completed", delivered_locally()), ("needs_review", delivered_by_review(true)), ("running", under_way()), ("paused", under_way())] {
        let Some(frame) = open_run(state, list) else { return };
        assert_tabs_agree(&frame);
    }
}
