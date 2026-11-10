//! The Orchestrations tab counts a project's runs and orders them by when they were last updated,
//! newest or oldest first.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn row(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": format!("feat-{id}"), "target_branch": "main", "state": state, "stage": null, "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"})
}

fn runs() -> Value {
    // Newest first, as the server sends them.
    json!({"status": 200, "json": {"runs": [row("a", "completed"), row("b", "running"), row("c", "failed"), row("d", "running"), row("e", "teleporting")]}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn order(frame: &Value) -> Vec<String> {
    texts(frame).into_iter().filter(|t| t.starts_with("feat-")).collect()
}

#[test]
fn the_tab_says_how_many_runs_there_are_and_how_many_are_running() {
    // The count sits beside the Run header and the running pill beside the page title.
    let Some(out) = run(json!([runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    let t = texts(&out["frames"][1]);
    assert!(t.iter().any(|t| t == "5") && t.iter().any(|t| t == "2 running"), "{t:?}");
    let Some(out) = run(json!([{"status": 200, "json": {"runs": [row("a", "running")]}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    let t = texts(&out["frames"][1]);
    assert!(t.iter().any(|t| t == "1") && t.iter().any(|t| t == "1 running"), "{t:?}");
    let Some(out) = run(json!([{"status": 200, "json": {"runs": []}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "0 running"), "an empty list still says none is running");
}

#[test]
fn the_updated_button_sorts_newest_first_and_each_click_flips_the_order() {
    let at = |id: &str, when: &str| json!({"run_id": id, "feature": format!("feat-{id}"), "target_branch": "main", "state": "running", "stage": null, "started_at": "2026-11-02T09:00:00Z", "updated_at": when});
    // The server's order is not the update order, and one run has no update time, so its start counts.
    let mut never = at("d", "x");
    never["updated_at"] = Value::Null;
    let rows = json!({"status": 200, "json": {"runs": [at("a", "2026-11-02T10:00:00Z"), at("b", "2026-11-02T12:00:00Z"), at("c", "2026-11-02T11:00:00Z"), never]}});
    let click = json!({"click": "button[data-run-sort]"});
    let Some(out) = run(json!([rows, projects()]), json!([{"submit": TOKEN}, click, click])) else { return };
    assert_eq!(order(&out["frames"][1]), ["feat-b", "feat-c", "feat-a", "feat-d"]);
    assert_eq!(order(&out["frames"][2]), ["feat-d", "feat-a", "feat-c", "feat-b"]);
    assert_eq!(order(&out["frames"][3]), ["feat-b", "feat-c", "feat-a", "feat-d"]);
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Updated"), "the button names what it sorts by");
}

#[test]
fn the_state_filter_offers_every_run_state_even_when_no_run_is_in_it() {
    let Some(out) = run(json!([{"status": 200, "json": {"runs": [row("a", "running")]}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    let t = texts(&out["frames"][1]);
    for label in ["State: All", "Running", "Pause requested", "Paused", "Needs review", "Blocked", "Failed", "Completed", "Merged"] {
        assert!(t.iter().any(|t| t == label), "{label} missing: {t:?}");
    }
}
