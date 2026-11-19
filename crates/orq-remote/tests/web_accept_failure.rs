//! A failed Accept's banner says what failed, lists each failing test by name with its own output (the first one
//! open), links to the Accept's full log and keeps the end of the output collapsed below; with no test named the
//! end of the output shows open. A failed verify gate on the timeline keeps its failing tests behind a closed line.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 12] = [
    "[data-accept-failure]",
    "[data-accept-failure] [data-failed-test]",
    "[data-accept-failure] details[data-failed-test][open]",
    "[data-accept-failure] [data-failed-test] code",
    "[data-failed-test-output]",
    "[data-failed-tests-more]",
    "a[data-accept-log][href$=\"/r-rev/logs/accept-2.log\"]",
    "[data-accept-log]",
    "details[data-accept-tail]",
    "details[data-accept-tail][open]",
    "[data-accept-output]",
    ".run-step [data-failed-test]",
];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn failed_run(failure: Value) -> Value {
    json!({"run_id": "r-rev", "feature": "f-r-rev", "state": "needs_review", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main", "accept_failure": failure})
}

fn detail(run: Value, events: Value) -> Value {
    let body = json!({"run": run, "events": events, "next_cursor": null, "remote_actions": true});
    let responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    self::run(json!({"url": "/projects/alpha/orchestrations/r-rev", "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS})).unwrap_or(Value::Null)
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

const LONG_NAME: &str = "orq-tui::ui_status_filter_names_and_a_very_long_module_path::unknown_statuses_are_listed_in_the_filter_after_the_known_ones";

fn two_tests() -> Value {
    json!([
        {"name": LONG_NAME, "output": "thread panicked at src/a.rs:1:1:\n  left: 1\n right: 2"},
        {"name": "orq-core::store saves", "output": "the store was empty"},
    ])
}

#[test]
fn the_banner_lists_each_failing_test_opening_its_output_links_the_full_log_and_keeps_the_tail_collapsed() {
    let failure = json!({"reason": "2 tests failed: a, b", "output": "the end of the run", "at": "2026-11-19T10:00:00Z", "log": "accept-2.log", "failed_tests": two_tests(), "failed_tests_more": 4});
    let out = detail(failed_run(failure), json!([]));
    if out.is_null() {
        return;
    }
    let end = last(&out);
    let all = texts(end);
    assert!(all.iter().any(|t| t == "Accept failed: 2 tests failed: a, b"), "{all:?}");
    assert_eq!(count(end, "[data-accept-failure] [data-failed-test]"), 2, "{out}");
    assert_eq!(count(end, "[data-accept-failure] [data-failed-test] code"), 2, "each name is monospace: {out}");
    assert_eq!(count(end, "[data-accept-failure] details[data-failed-test][open]"), 1, "only the first is open: {out}");
    assert_eq!(count(end, "[data-failed-test-output]"), 2, "{out}");
    assert!(all.iter().any(|t| t == LONG_NAME), "{all:?}");
    assert!(all.iter().any(|t| t == "the store was empty"), "{all:?}");
    assert_eq!(count(end, "[data-failed-tests-more]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "and 4 more"), "{all:?}");
    assert_eq!(count(end, "a[data-accept-log][href$=\"/r-rev/logs/accept-2.log\"]"), 1, "the full log link: {out}");
    assert!(all.iter().any(|t| t == "Open the full log"), "{all:?}");
    assert_eq!(count(end, "details[data-accept-tail]"), 1, "{out}");
    assert_eq!(count(end, "details[data-accept-tail][open]"), 0, "the tail is collapsed when tests are named: {out}");
}

#[test]
fn with_no_test_names_the_tail_shows_open_and_no_log_named_gives_no_link() {
    let failure = json!({"reason": "`cargo test` exited with status 101", "output": "error: could not compile `orq-core`", "at": "2026-11-19T10:00:00Z", "failed_tests": [], "failed_tests_more": 0});
    let out = detail(failed_run(failure), json!([]));
    if out.is_null() {
        return;
    }
    let end = last(&out);
    assert_eq!(count(end, "[data-accept-failure] [data-failed-test]"), 0, "{out}");
    assert_eq!(count(end, "details[data-accept-tail][open]"), 1, "{out}");
    assert_eq!(count(end, "[data-accept-output]"), 1, "{out}");
    assert_eq!(count(end, "[data-accept-log]"), 0, "{out}");
}

#[test]
fn a_failed_verify_gate_on_the_timeline_keeps_its_failing_tests_behind_a_closed_line() {
    let run = json!({"run_id": "r-rev", "feature": "f-r-rev", "state": "failed", "stage": "verify", "started_at": null, "updated_at": null, "target_branch": "main"});
    let events = json!([
        {"event_id": "e-1", "kind": "stage.started", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-19T10:00:00Z"},
        {"event_id": "e-2", "kind": "stage.failed", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-19T10:01:00Z", "failure": {"reason": "2 tests failed", "failed_tests": two_tests(), "failed_tests_more": 0}},
    ]);
    let out = detail(run, events);
    if out.is_null() {
        return;
    }
    let end = last(&out);
    // The banner's open list is not the timeline's: there they sit behind one closed line (web_timeline_failed_tests).
    assert_eq!(count(end, ".run-step [data-failed-test]"), 0, "{out}");
    assert!(texts(end).iter().any(|t| t == "Failed tests: 2"), "{out}");
    assert!(!texts(end).iter().any(|t| t == "orq-core::store saves"), "{out}");
}
