//! The Dashboard: one card per project with its counts; choosing a card opens the project.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [
        {"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 2, "reviews_waiting": 1, "failed_runs": 4, "failed_window_hours": 24},
        {"id": "broken", "name": "broken", "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": 24}
    ]}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn the_dashboard_asks_for_the_projects_with_the_held_token_and_shows_one_card_each_with_its_counts() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let frame = &out["frames"][1];
    assert_eq!(frame["settled"]["path"], "/dashboard");
    assert_eq!(frame["settled"]["cards"], 2, "{out}");
    let text = shown(frame);
    for expected in ["Features: 3", "Active runs: 2", "Reviews waiting: 1", "Failed runs (last 24h): 4"] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
}

#[test]
fn a_project_that_cannot_be_read_is_listed_as_unavailable_without_counts() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Unavailable"), "{text}");
    assert_eq!(text.matches("Features:").count(), 1, "only the readable project has counts: {text}");
}

#[test]
fn choosing_a_project_opens_it() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha", "{out}");
}

#[test]
fn no_projects_says_so() {
    let Some(out) = run(json!([{"status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).contains("No projects found."), "{out}");
}
