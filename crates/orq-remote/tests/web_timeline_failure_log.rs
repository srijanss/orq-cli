//! A failed step says why it failed and shows the log lines around the error, and its "See the logs"
//! opens that log at the error line, highlighted and not following. The Logs tab opened on its own starts
//! at the latest failure's log, and the Events tab links a failure to its log too.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";
const API: &str = "/api/projects/alpha/runs/run-1";

const EXCERPT_TOGGLE: &str = "[data-step-excerpt-toggle]";

fn selectors() -> Vec<&'static str> {
    vec![
        "[data-step-excerpt]",
        EXCERPT_TOGGLE,
        "[data-step-excerpt-toggle][aria-expanded=\"false\"]",
        "[data-step-excerpt-toggle][aria-expanded=\"true\"]",
        ".run-step:first-child [data-step-excerpt]",
        ".run-step:first-child [data-step-excerpt-toggle][aria-expanded=\"false\"]",
        ".run-step:last-child [data-step-excerpt]",
        ".run-step:first-child .run-step-failure",
        ".log-line[data-log-mark]",
        "button[data-log-follow][aria-checked=false]",
        "button[data-log-follow][aria-checked=true]",
        "button[data-event-log]",
    ]
}

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": selectors()});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn stream() -> Value {
    json!({"status": 200, "stream": [], "end": "hold"})
}

fn timeline(events: Value) -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": "verify", "started_at": null, "updated_at": null});
    json!({"status": 200, "json": {"run": header, "events": events, "next_cursor": null}})
}

fn event(id: &str, kind: &str, stage: &str, attempt: u32, failure: Option<Value>) -> Value {
    let mut event = json!({"event_id": id, "kind": kind, "stage": stage, "attempt": attempt, "occurred_at": "2026-11-16T15:27:21Z"});
    if let Some(failure) = failure {
        event["failure"] = failure;
    }
    event
}

/// A gate that failed once at `verify.log` line 42, after an implement attempt that failed in its own log.
fn failed_gate(log: Value) -> Value {
    timeline(json!([
        event("e1", "stage.started", "implement_and_review", 1, None),
        event("e2", "stage.failed", "implement_and_review", 1, Some(json!({"reason": "the session exited", "log": {"file": "implement.log", "line": 3, "excerpt": "x"}}))),
        event("e3", "stage.started", "verify", 2, None),
        event("e4", "stage.failed", "verify", 2, Some(json!({"reason": "step 2 exited with status 101", "log": log}))),
    ]))
}

fn gate_log() -> Value {
    json!({"file": "verify.log", "line": 42, "excerpt": "thread 'main' panicked at src/lib.rs:7:5\n<b>boom</b>"})
}

fn files() -> Value {
    json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}, {"name": "verify.log", "bytes": 900}]}})
}

/// Lines 38..=50 of a log, as the page after line 37 gives them.
fn page_after_37() -> Value {
    let lines: Vec<String> = (38..=50).map(|n| if n == 42 { "thread 'main' panicked at src/lib.rs:7:5".to_string() } else { format!("line {n}") }).collect();
    json!({"status": 200, "json": {"name": "verify.log", "lines": lines, "next_cursor": null, "truncated": false}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn a_failed_step_says_why_with_the_lines_around_the_error_and_opens_its_log_there() {
    let responses = json!([failed_gate(gate_log()), projects(), stream(), stream(), files(), page_after_37()]);
    let steps = json!([{"submit": TOKEN}, {"click": ".run-step:last-child button[data-step-logs]"}]);
    let Some(out) = run(RUN, responses, steps) else { return };

    let shown = &out["frames"][1];
    let t = texts(shown);
    assert!(t.iter().any(|x| x.starts_with("Failed: step 2 exited with status 101")), "{t:?}");
    assert_eq!(count(shown, "[data-step-excerpt]"), 1, "only the step the run failed on has its excerpt open: {out}");
    // The excerpt is text, so markup in a log line is shown as written.
    assert!(t.iter().any(|x| x.contains("panicked at src/lib.rs:7:5\n<b>boom</b>")), "{t:?}");

    let opened = &out["frames"][2];
    assert_eq!(opened["settled"]["path"], format!("{RUN}/logs/verify.log/after/37/line/42"), "{out}");
    assert!(urls(&out).contains(&format!("{API}/logs/verify.log/after/37")), "{:?}", urls(&out));
    assert_eq!(count(opened, ".log-line[data-log-mark]"), 1, "{out}");
    assert_eq!(count(opened, "button[data-log-follow][aria-checked=false]"), 1, "an opened error is not followed away from: {out}");
    assert!(opened["settled"]["scrolled"].as_array().unwrap().iter().any(|id| id == "log-mark"), "the error line is scrolled into view: {out}");
}

#[test]
fn an_earlier_failed_step_keeps_its_failure_line_and_its_log_lines_behind_a_closed_line() {
    let steps = json!([{"submit": TOKEN}, {"click": ".run-step:first-child [data-step-excerpt-toggle]"}]);
    let Some(out) = run(RUN, json!([failed_gate(gate_log()), projects(), stream()]), steps) else { return };

    let shown = &out["frames"][1];
    assert_eq!(count(shown, ".run-step:first-child .run-step-failure"), 1, "the failure line stays visible: {out}");
    assert_eq!(count(shown, ".run-step:first-child [data-step-excerpt]"), 0, "{out}");
    assert_eq!(count(shown, ".run-step:first-child [data-step-excerpt-toggle][aria-expanded=\"false\"]"), 1, "{out}");
    assert!(texts(shown).iter().any(|t| t == "Log lines at the error"), "{:?}", texts(shown));
    assert_eq!(count(shown, ".run-step:last-child [data-step-excerpt]"), 1, "the step the run failed on stays open: {out}");
    assert_eq!(count(shown, "[data-step-excerpt-toggle][aria-expanded=\"true\"]"), 1, "{out}");

    let opened = &out["frames"][2];
    assert_eq!(count(opened, ".run-step:first-child [data-step-excerpt]"), 1, "opening it shows the excerpt: {out}");
    assert_eq!(count(opened, "[data-step-excerpt]"), 2, "{out}");
}

#[test]
fn a_run_that_did_not_end_failed_keeps_every_excerpt_closed() {
    let mut page = failed_gate(gate_log());
    page["json"]["run"]["state"] = json!("running");
    let Some(out) = run(RUN, json!([page, projects(), stream()]), json!([{"submit": TOKEN}])) else { return };
    let shown = &out["frames"][1];
    assert_eq!(count(shown, "[data-step-excerpt]"), 0, "{out}");
    assert_eq!(count(shown, "[data-step-excerpt-toggle][aria-expanded=\"false\"]"), 2, "{out}");
}

#[test]
fn a_running_run_that_turns_failed_on_a_refresh_opens_the_step_it_failed_on_unless_its_line_was_closed() {
    let mut running = failed_gate(gate_log());
    running["json"]["run"]["state"] = json!("running");
    let responses = json!([running, projects(), stream(), failed_gate(gate_log()), stream()]);
    let Some(out) = run(RUN, responses.clone(), json!([{"submit": TOKEN}, {"tick": 30000}])) else { return };
    assert_eq!(count(&out["frames"][1], "[data-step-excerpt]"), 0, "{out}");
    let refreshed = &out["frames"][2];
    assert_eq!(count(refreshed, ".run-step:last-child [data-step-excerpt]"), 1, "the step the run failed on opens once the run is failed: {out}");
    assert_eq!(count(refreshed, "[data-step-excerpt]"), 1, "{out}");

    // A line the reader opened and closed again stays as they left it.
    let steps = json!([{"submit": TOKEN}, {"click": EXCERPT_TOGGLE, "index": 1}, {"click": EXCERPT_TOGGLE, "index": 1}, {"tick": 30000}]);
    let Some(out) = run(RUN, responses, steps) else { return };
    let refreshed = &out["frames"][4];
    assert_eq!(count(refreshed, "[data-step-excerpt]"), 0, "the reader's choice is kept: {out}");
}

#[test]
fn an_opened_error_address_highlights_its_line_and_a_filter_drops_the_highlight() {
    let responses = json!([failed_gate(gate_log()), projects(), stream(), files(), page_after_37()]);
    let steps = json!([{"submit": TOKEN}, {"set": "input[data-log-filter]", "value": "line"}]);
    let Some(out) = run(&format!("{RUN}/logs/verify.log/after/37/line/42"), responses, steps) else { return };
    assert_eq!(urls(&out)[4], format!("{API}/logs/verify.log/after/37"), "{out}");
    assert_eq!(count(&out["frames"][1], ".log-line[data-log-mark]"), 1, "{out}");
    assert_eq!(count(&out["frames"][2], ".log-line[data-log-mark]"), 0, "a filtered list has no line to point at: {out}");

    for odd in ["after/37/line/abc", "line/42", "after/37/line/0"] {
        let Some(out) = run(&format!("{RUN}/logs/verify.log/{odd}"), json!([projects()]), json!([{"submit": TOKEN}])) else { return };
        assert_eq!(urls(&out).iter().filter(|u| u.contains("/runs")).count(), 0, "{odd}: {out}");
    }
}

#[test]
fn an_error_near_the_start_opens_the_first_page_and_a_failure_without_a_line_opens_its_file() {
    let early = json!({"file": "verify.log", "line": 2, "excerpt": "error: could not compile"});
    let first = json!({"status": 200, "json": {"name": "verify.log", "lines": ["one", "error: could not compile", "three"], "next_cursor": null, "truncated": false}});
    let steps = json!([{"submit": TOKEN}, {"click": ".run-step:last-child button[data-step-logs]"}]);
    let Some(out) = run(RUN, json!([failed_gate(early), projects(), stream(), stream(), files(), first]), steps.clone()) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], format!("{RUN}/logs/verify.log/after/0/line/2"), "{out}");
    assert_eq!(count(&out["frames"][2], ".log-line[data-log-mark]"), 1, "{out}");

    let unplaced = json!({"file": "verify.log", "excerpt": "tail"});
    let page = json!({"status": 200, "json": {"name": "verify.log", "lines": ["tail"], "next_cursor": null, "truncated": false}});
    let Some(out) = run(RUN, json!([failed_gate(unplaced), projects(), stream(), stream(), files(), page]), steps) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], format!("{RUN}/logs/verify.log"), "{out}");
    assert!(urls(&out).contains(&format!("{API}/logs/verify.log")), "{:?}", urls(&out));
    assert_eq!(count(&out["frames"][2], ".log-line[data-log-mark]"), 0, "{out}");
}

#[test]
fn a_failure_without_a_log_keeps_the_plain_logs_link_and_shows_no_excerpt() {
    let events = timeline(json!([
        event("e1", "stage.started", "verify", 1, None),
        event("e2", "stage.failed", "verify", 1, Some(json!({"reason": "no steps ran"}))),
    ]));
    let list = json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}]}});
    let page = json!({"status": 200, "json": {"name": "implement.log", "lines": ["x"], "next_cursor": null, "truncated": false}});
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-step-logs]"}]);
    let Some(out) = run(RUN, json!([events, projects(), stream(), list, page]), steps) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x.starts_with("Failed: no steps ran")), "{out}");
    assert_eq!(count(&out["frames"][1], "[data-step-excerpt]"), 0, "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], RUN, "{out}");
    assert_eq!(urls(&out)[4], format!("{API}/logs/implement.log"), "{out}");
    assert_eq!(count(&out["frames"][2], "button[data-log-follow][aria-checked=true]"), 1, "{out}");

    // A failure the server gave nothing for still says what failed.
    let bare = timeline(json!([event("e1", "stage.started", "verify", 1, None), event("e2", "stage.failed", "verify", 1, None)]));
    let Some(out) = run(RUN, json!([bare, projects(), stream()]), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x.starts_with("Failed: stage.failed")), "{out}");
}

#[test]
fn the_logs_tab_opens_the_latest_failures_log_rather_than_the_first_file() {
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}]);
    let page = json!({"status": 200, "json": {"name": "verify.log", "lines": ["x"], "next_cursor": null, "truncated": false}});
    let Some(out) = run(RUN, json!([failed_gate(gate_log()), projects(), stream(), files(), page]), steps.clone()) else { return };
    assert_eq!(urls(&out)[4], format!("{API}/logs/verify.log"), "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], RUN, "{out}");

    // A latest failure whose log is not listed leaves the first file.
    let gone = json!({"file": "gone.log", "line": 4, "excerpt": "x"});
    let page = json!({"status": 200, "json": {"name": "implement.log", "lines": ["x"], "next_cursor": null, "truncated": false}});
    let Some(out) = run(RUN, json!([failed_gate(gone), projects(), stream(), files(), page]), steps) else { return };
    assert_eq!(urls(&out)[4], format!("{API}/logs/implement.log"), "{out}");
}

#[test]
fn the_events_tab_gives_a_failure_its_reason_and_a_link_to_its_log_at_the_error() {
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-run-tab=events]"}, {"click": ".attempt:last-child button[data-event-log]"}]);
    let Some(out) = run(RUN, json!([failed_gate(gate_log()), projects(), stream(), stream(), files(), page_after_37()]), steps) else { return };
    let events = &out["frames"][2];
    assert_eq!(count(events, "button[data-event-log]"), 2, "{out}");
    let t = texts(events);
    assert!(t.iter().any(|x| x == "step 2 exited with status 101"), "{t:?}");
    assert!(t.iter().any(|x| x == "Open log at the error"), "{t:?}");
    assert_eq!(out["frames"][3]["settled"]["path"], format!("{RUN}/logs/verify.log/after/37/line/42"), "{out}");
    assert_eq!(count(&out["frames"][3], ".log-line[data-log-mark]"), 1, "{out}");
}

#[test]
fn an_unusable_failure_is_shown_as_a_plain_failure() {
    for odd in [
        json!("text"),
        json!({"reason": 5}),
        json!({"reason": "r", "log": {"file": "../x.log", "line": 3, "excerpt": "e"}}),
        json!({"reason": "r", "log": {"file": "verify.log", "line": -1, "excerpt": "e"}}),
        json!({"reason": "r", "log": {"file": "verify.log", "line": 3, "excerpt": 7}}),
    ] {
        let events = timeline(json!([event("e1", "stage.started", "verify", 1, None), event("e2", "stage.failed", "verify", 1, Some(odd.clone()))]));
        let Some(out) = run(RUN, json!([events, projects(), stream()]), json!([{"submit": TOKEN}])) else { return };
        let frame = &out["frames"][1];
        assert_eq!(count(frame, "[data-step-excerpt]"), 0, "{odd}: {out}");
        assert_eq!(count(frame, "button[data-event-log]"), 0, "{odd}: {out}");
        assert!(texts(frame).iter().any(|x| x.starts_with("Failed: ")), "{odd}: {out}");
    }
}
