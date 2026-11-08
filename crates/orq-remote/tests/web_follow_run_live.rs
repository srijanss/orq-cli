//! A run page that is at the end of its events keeps following them: new events appear as they are
//! sent, and a dropped stream is reopened from the last event seen without losing or repeating any.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn event(id: &str, kind: &str) -> Value {
    json!({"event_id": id, "kind": kind, "stage": null, "attempt": 1, "occurred_at": null})
}

fn timeline(events: Vec<Value>, next: Value) -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": null, "started_at": null, "updated_at": null});
    json!({"status": 200, "json": {"run": header, "events": events, "next_cursor": next}})
}

fn sse(id: &str, kind: &str) -> String {
    format!("id: {id}\nevent: event\ndata: {}\n\n", event(id, kind))
}

fn stream(chunks: Vec<String>, end: &str) -> Value {
    json!({"status": 200, "stream": chunks, "end": end})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, text: &str) -> usize {
    texts(frame).iter().filter(|t| t.as_str() == text).count()
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_run_at_its_last_page_follows_new_events_from_its_last_event_with_the_token() {
    // The second event is split across two chunks; a comment line and a broken event are skipped.
    let second = sse("e1", "stage.finished");
    let (a, b) = second.split_at(20);
    let chunks = vec![": connected\n\n".to_string(), a.to_string(), b.to_string(), "id: bad\nevent: event\ndata: {nope\n\n".to_string()];
    let responses = json!([timeline(vec![event("e0", "stage.started")], Value::Null), projects(), stream(chunks, "hold")]);
    let Some(out) = run(RUN, responses, json!([{"submit": TOKEN}])) else { return };
    assert_eq!(urls(&out)[2], "/api/projects/alpha/runs/run-1/events/after/e0", "{out}");
    assert_eq!(out["requests"][2]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "stage.started"), 1, "{:?}", texts(frame));
    assert_eq!(count(frame, "stage.finished"), 1, "{:?}", texts(frame));
    assert!(texts(frame).iter().any(|t| t == "Following live"), "{:?}", texts(frame));
}

#[test]
fn a_dropped_stream_is_reopened_from_the_last_event_seen_without_repeating_any() {
    let first = stream(vec![sse("e1", "one")], "error");
    // The reopened stream sends e1 again before the new one.
    let second = stream(vec![sse("e1", "one"), sse("e2", "two")], "hold");
    let responses = json!([timeline(vec![event("e0", "zero")], Value::Null), projects(), first, second]);
    let Some(out) = run(RUN, responses, json!([{"submit": TOKEN}, {"fire": 3000}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Reconnecting…"), "{:?}", texts(&out["frames"][1]));
    assert_eq!(out["frames"][1]["settled"]["retries"], 1, "{out}");
    assert_eq!(urls(&out)[3], "/api/projects/alpha/runs/run-1/events/after/e1", "{out}");
    let frame = &out["frames"][2];
    assert_eq!((count(frame, "zero"), count(frame, "one"), count(frame, "two")), (1, 1, 1), "{:?}", texts(frame));
    assert!(texts(frame).iter().any(|t| t == "Following live"));
}

#[test]
fn an_empty_run_follows_from_the_start_and_a_page_with_more_pages_does_not_follow() {
    let responses = json!([timeline(vec![], Value::Null), projects(), stream(vec![sse("e0", "first")], "hold")]);
    let Some(out) = run(RUN, responses, json!([{"submit": TOKEN}])) else { return };
    assert_eq!(urls(&out)[2], "/api/projects/alpha/runs/run-1/events", "{out}");
    assert_eq!(count(&out["frames"][1], "first"), 1);

    let responses = json!([timeline(vec![event("e0", "zero")], json!("e0")), projects()]);
    let Some(out) = run(RUN, responses, json!([{"submit": TOKEN}])) else { return };
    assert!(urls(&out).iter().all(|u| !u.contains("/events")), "{out}");
}

#[test]
fn a_refused_stream_signs_out_a_missing_one_stops_and_leaving_the_page_closes_it() {
    let base = |tail: Value| json!([timeline(vec![event("e0", "zero")], Value::Null), projects(), tail]);
    let Some(out) = run(RUN, base(json!({"status": 401, "json": {}})), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Open the inbox"), "{:?}", texts(&out["frames"][1]));

    let Some(out) = run(RUN, base(json!({"status": 404, "json": {}})), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["retries"], 0, "{out}");
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Live updates stopped"), "{:?}", texts(&out["frames"][1]));

    let Some(out) = run(RUN, base(stream(vec![], "hold")), json!([{"submit": TOKEN}, {"click": "button[data-back=list]", "index": 0}])) else { return };
    assert_eq!(out["requests"][2]["aborted"], true, "{out}");
}

#[test]
fn an_event_repeated_within_one_read_is_shown_once() {
    let both = format!("{}{}{}", sse("e1", "one"), sse("e1", "one"), sse("e2", "two"));
    let responses = json!([timeline(vec![event("e0", "zero")], Value::Null), projects(), stream(vec![both], "hold")]);
    let Some(out) = run(RUN, responses, json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!((count(frame, "zero"), count(frame, "one"), count(frame, "two")), (1, 1, 1), "{:?}", texts(frame));
}
