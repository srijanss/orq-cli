//! Opening a run shows its timeline: the run's header and its events in order, a page at a time.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

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

fn header() -> Value {
    json!({"run_id": "run-1", "feature": "<b>login</b>", "target_branch": "main", "state": "failed", "stage": "implement", "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"})
}

fn event(id: &str, kind: &str, stage: Value, at: &str) -> Value {
    json!({"event_id": id, "kind": kind, "stage": stage, "attempt": 2, "occurred_at": at})
}

fn page(events: Value, next: Value) -> Value {
    json!({"status": 200, "json": {"run": header(), "events": events, "next_cursor": next}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_run_page_shows_the_header_and_the_events_in_order_as_text() {
    let events = json!([event("e0", "run.started", Value::Null, "2026-11-02T10:00:00Z"), event("e1", "stage.<i>started</i>", json!("implement"), "2026-11-02T10:05:00Z")]);
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([page(events, Value::Null), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/runs/run-1", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let t = texts(&out["frames"][1]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(t.iter().any(|x| x == "<b>login</b>") && t.iter().any(|x| x == "Failed") && t.iter().any(|x| x == "Stage: implement"), "{t:?}");
    assert!(at("run.started") < at("stage.<i>started</i>"), "{t:?}");
    assert!(at("2026-11-02T10:00:00Z") < at("2026-11-02T10:05:00Z") && t.iter().any(|x| x == "Attempt 2"), "{t:?}");
    assert!(!t.iter().any(|x| x.contains("Load more")), "no cursor, no more: {t:?}");
}

#[test]
fn a_full_page_offers_the_next_one_by_its_cursor_and_the_run_row_opens_the_page() {
    let first = page(json!([event("e0", "run.started", Value::Null, "2026-11-02T10:00:00Z")]), json!("e0"));
    let second = page(json!([event("e1", "run.failed", Value::Null, "2026-11-02T11:00:00Z")]), Value::Null);
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([first, projects(), second]), json!([{"submit": TOKEN}, {"click": "button[data-more]", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/orchestrations/run-1/after/e0", "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/alpha/runs/run-1/after/e0", "{out}");
    let t = texts(&out["frames"][2]);
    assert!(t.iter().any(|x| x == "run.failed") && !t.iter().any(|x| x == "run.started"), "{t:?}");

    let list = json!({"status": 200, "json": {"runs": [header()]}});
    let events = page(json!([event("e0", "run.started", Value::Null, "2026-11-02T10:00:00Z")]), Value::Null);
    let Some(out) = run("/projects/alpha/orchestrations", json!([list, projects(), events]), json!([{"submit": TOKEN}, {"click": "button[data-open]", "index": 0}, {"click": "button[data-back]", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/orchestrations/run-1", "{out}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/alpha/orchestrations", "{out}");
}

#[test]
fn unusable_data_draws_no_events_and_an_odd_address_is_not_found() {
    let bad = json!({"status": 200, "json": {"run": header(), "events": [{"event_id": 5}], "next_cursor": null}});
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([bad, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(!texts(&out["frames"][1]).iter().any(|x| x == "<b>login</b>"), "{out}");
    let Some(out) = run("/projects/alpha/orchestrations/a%20b", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"].as_str().unwrap_or("").contains("/runs")).count(), 0, "{out}");
}
