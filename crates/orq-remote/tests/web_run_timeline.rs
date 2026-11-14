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
    assert!(t.iter().any(|x| x == "<b>login</b>") && t.iter().any(|x| x == "Failed"), "{t:?}");
    assert!(!t.iter().any(|x| x.starts_with("Run run-1")), "no run number line above the tabs: {t:?}");
    assert!(at("run.started") < at("stage.<i>started</i>"), "{t:?}");
    let moments: Vec<&str> = out["frames"][1]["settled"]["times"].as_array().unwrap().iter().filter(|m| m["label"] == "").filter_map(|m| m["datetime"].as_str()).collect();
    assert_eq!(moments, ["2026-11-02T10:00:00Z", "2026-11-02T10:05:00Z"], "{out}");
    assert!(t.iter().any(|x| x == "Attempt 2"), "{t:?}");
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

#[test]
fn the_run_card_has_a_back_arrow_timeline_and_logs_tabs_and_each_step_shows_how_long_it_took() {
    let started = json!({"event_id": "e0", "kind": "stage.started", "stage": "implement", "attempt": 1, "occurred_at": "2026-11-02T10:00:00Z"});
    let done = json!({"event_id": "e1", "kind": "stage.completed", "stage": "implement", "attempt": 1, "occurred_at": "2026-11-02T10:02:30Z"});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([page(json!([started, done]), json!("e1")), projects(), stream]), json!([{"submit": TOKEN}])) else { return };
    let t = texts(&out["frames"][1]);
    assert!(t.iter().any(|x| x == "Timeline") && t.iter().any(|x| x == "Logs"), "{t:?}");
    assert!(t.iter().any(|x| x == "2m 30s"), "the step's duration: {t:?}");
    assert!(t.iter().any(|x| x == "Connected") && t.iter().any(|x| x == "Project:"), "{t:?}");
}

/// A run's page says how the run delivers its work, and once delivered, how; its status pill is
/// Completed whether the run was pushed, merged locally or merged by hand.
#[test]
fn a_run_page_shows_its_delivery_mode_and_a_delivered_runs_ending() {
    let delivered = |delivery: &str, state: &str, ending: Value| {
        let mut head = header();
        head["delivery"] = json!(delivery);
        head["state"] = json!(state);
        head["ending"] = ending;
        json!({"status": 200, "json": {"run": head, "events": [], "next_cursor": null}})
    };
    for (delivery, state, ending, line, pill) in [
        ("review", "needs_review", Value::Null, "Open review", "Needs review"),
        ("push", "completed", json!("pushed"), "Pushed", "Completed"),
        ("merge_local", "completed", json!("merged"), "Merged locally", "Completed"),
        ("merge_local", "running", Value::Null, "Merge locally", "Running"),
        ("push", "failed", Value::Null, "Push only", "Failed"),
        ("merge_local", "completed", json!("teleported"), "Merge locally", "Completed"),
        ("review", "merged", Value::Null, "Merged by hand", "Completed"),
    ] {
        let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([delivered(delivery, state, ending.clone()), projects()]), json!([{"submit": TOKEN}])) else { return };
        let t = texts(&out["frames"][1]);
        assert!(t.iter().any(|x| x == &format!("Delivery: {line}")), "{delivery} {state} {ending}: {t:?}");
        assert!(t.iter().any(|x| x == pill), "{delivery} {state} {ending}: {t:?}");
        assert!(!t.iter().any(|x| x == "Merged" || x == "Pushed" || x == "Merged locally"), "no pill names the ending: {t:?}");
    }
    // A run whose delivery the page does not know, or that has none, shows no delivery line.
    for delivery in [json!("teleport"), Value::Null] {
        let mut head = header();
        head["delivery"] = delivery.clone();
        let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([{"status": 200, "json": {"run": head, "events": [], "next_cursor": null}}, projects()]), json!([{"submit": TOKEN}])) else { return };
        let t = texts(&out["frames"][1]);
        assert!(!t.iter().any(|x| x.starts_with("Delivery")), "{delivery}: {t:?}");
        assert!(t.iter().any(|x| x == "<b>login</b>"), "the run is still shown: {t:?}");
    }
}
