//! The run timeline nests retry attempts with their own times and failures, so a later success
//! never hides an earlier failure, and numbers each review round.

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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn event(id: &str, kind: &str, stage: &str, attempt: u32, at: &str) -> Value {
    json!({"event_id": id, "kind": kind, "stage": stage, "attempt": attempt, "occurred_at": at})
}

fn page(events: Vec<Value>) -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "review", "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T11:00:00Z"});
    json!({"status": 200, "json": {"run": header, "events": events, "next_cursor": null}})
}

fn open(events: Vec<Value>) -> Option<Value> {
    run("/projects/alpha/orchestrations/run-1", json!([page(events), projects()]), json!([{"submit": TOKEN}]))
}

fn attempts(out: &Value) -> &Vec<Value> {
    out["frames"][1]["settled"]["attempts"].as_array().unwrap()
}

#[test]
fn a_failed_attempt_stays_visible_beside_the_later_attempt_that_succeeded() {
    let Some(out) = open(vec![
        event("e1", "stage.started", "implement", 1, "2026-11-02T10:00:00Z"),
        event("e2", "stage.failed", "implement", 1, "2026-11-02T10:05:00Z"),
        event("e3", "stage.started", "implement", 2, "2026-11-02T10:06:00Z"),
        event("e4", "stage.completed", "implement", 2, "2026-11-02T10:20:00Z"),
    ]) else { return };
    let groups = attempts(&out);
    assert_eq!(groups.len(), 2, "{out}");
    assert_eq!(groups[0]["title"], "implement, attempt 1", "{out}");
    assert_eq!(groups[0]["failed"], true, "{out}");
    assert_eq!(groups[0]["events"], json!(["stage.started", "stage.failed"]), "{out}");
    assert!(groups[0]["text"].as_str().unwrap().contains("Failed"), "{out}");
    assert_eq!(groups[1]["title"], "implement, attempt 2", "{out}");
    assert_eq!(groups[1]["failed"], false, "{out}");
    assert_eq!(groups[1]["events"], json!(["stage.started", "stage.completed"]), "{out}");
    // Each attempt carries its own times.
    assert_ne!(groups[0]["times"], groups[1]["times"], "{out}");
    assert!(!groups[0]["times"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn review_rounds_are_numbered_in_the_order_they_started() {
    let Some(out) = open(vec![
        event("e1", "stage.started", "review", 1, "2026-11-02T10:00:00Z"),
        event("e2", "stage.completed", "review", 1, "2026-11-02T10:10:00Z"),
        event("e3", "stage.started", "implement", 1, "2026-11-02T10:11:00Z"),
        event("e4", "stage.completed", "implement", 1, "2026-11-02T10:20:00Z"),
        event("e5", "stage.started", "review", 2, "2026-11-02T10:21:00Z"),
        event("e6", "stage.completed", "review", 2, "2026-11-02T10:30:00Z"),
    ]) else { return };
    let titles: Vec<&str> = attempts(&out).iter().map(|g| g["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Review round 1", "implement, attempt 1", "Review round 2"], "{out}");
}

#[test]
fn events_with_no_stage_stay_in_order_outside_any_attempt() {
    let Some(out) = open(vec![
        json!({"event_id": "e0", "kind": "run.launched", "stage": null, "attempt": 0, "occurred_at": "2026-11-02T10:00:00Z"}),
        event("e1", "stage.started", "implement", 1, "2026-11-02T10:01:00Z"),
        json!({"event_id": "e2", "kind": "run.completed", "stage": null, "attempt": 0, "occurred_at": "2026-11-02T10:30:00Z"}),
    ]) else { return };
    assert_eq!(attempts(&out).len(), 1, "{out}");
    let all = out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ");
    let launched = all.find("run.launched").unwrap();
    let started = all.find("stage.started").unwrap();
    let completed = all.find("run.completed").unwrap();
    assert!(launched < started && started < completed, "{all}");
}

#[test]
fn an_event_with_no_stage_inside_an_attempt_keeps_it_in_one_group_in_order() {
    let Some(out) = open(vec![
        event("e1", "stage.started", "review", 1, "2026-11-02T10:00:00Z"),
        json!({"event_id": "e2", "kind": "session.note", "stage": null, "attempt": 0, "occurred_at": "2026-11-02T10:01:00Z"}),
        event("e3", "stage.failed", "review", 1, "2026-11-02T10:02:00Z"),
        event("e4", "stage.started", "review", 1, "2026-11-02T10:03:00Z"),
        event("e5", "stage.completed", "review", 1, "2026-11-02T10:04:00Z"),
    ]) else { return };
    let groups = attempts(&out);
    assert_eq!(groups.len(), 2, "a retry with the same attempt number is its own round: {out}");
    assert_eq!(groups[0]["title"], "Review round 1", "{out}");
    assert_eq!(groups[0]["failed"], true, "{out}");
    assert_eq!(groups[0]["events"], json!(["stage.started", "session.note", "stage.failed"]), "{out}");
    let text = groups[0]["text"].as_str().unwrap();
    assert!(text.find("stage.started").unwrap() < text.find("session.note").unwrap() && text.find("session.note").unwrap() < text.find("stage.failed").unwrap(), "{text}");
    assert_eq!(groups[1]["title"], "Review round 2", "{out}");
    assert_eq!(groups[1]["failed"], false, "{out}");
}

#[test]
fn a_later_page_names_its_review_round_from_the_run_not_from_the_page() {
    let mut late = event("e9", "stage.started", "review", 1, "2026-11-02T12:00:00Z");
    late["review_round"] = json!(3);
    let Some(out) = open(vec![late, event("e10", "stage.completed", "review", 1, "2026-11-02T12:05:00Z")]) else { return };
    assert_eq!(attempts(&out)[0]["title"], "Review round 3", "{out}");
}

#[test]
fn a_page_that_begins_mid_round_keeps_that_round_apart_from_the_next_one() {
    let mut cont = event("e5", "stage.completed", "review", 1, "2026-11-02T10:00:00Z");
    cont["review_round"] = json!(2);
    let mut start = event("e6", "stage.started", "review", 1, "2026-11-02T10:10:00Z");
    start["review_round"] = json!(3);
    let mut fail = event("e7", "stage.failed", "review", 1, "2026-11-02T10:12:00Z");
    fail["review_round"] = json!(3);
    let Some(out) = open(vec![cont, start, fail]) else { return };
    let groups = attempts(&out);
    assert_eq!(groups.len(), 2, "{out}");
    assert_eq!(groups[0]["title"], "Review round 2", "{out}");
    assert_eq!(groups[0]["failed"], false, "{out}");
    assert_eq!(groups[1]["title"], "Review round 3", "{out}");
    assert_eq!(groups[1]["failed"], true, "{out}");
}

#[test]
fn live_review_numbering_carries_on_from_the_runs_own_count() {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": "implement", "started_at": null, "updated_at": null});
    let fetched = json!({"status": 200, "json": {"run": header, "events": [event("e1", "stage.started", "implement", 1, "2026-11-02T10:00:00Z")], "review_rounds": 2, "next_cursor": null}});
    let live = format!("id: e2\nevent: event\ndata: {}\n\n", event("e2", "stage.started", "review", 1, "2026-11-02T10:05:00Z"));
    let stream = json!({"status": 200, "stream": [live], "end": "hold"});
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([fetched, projects(), stream]), json!([{"submit": TOKEN}])) else { return };
    let titles: Vec<&str> = attempts(&out).iter().map(|g| g["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["implement, attempt 1", "Review round 3"], "{out}");
}
