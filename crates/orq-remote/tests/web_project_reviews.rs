//! A project's Reviews tab: the review list and the report open under the project's own address
//! and read the project's own endpoints.

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

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [
        {"run_id": "run-1", "feature": "first-feature", "status": "approved", "risk": "low", "stale": false},
        {"run_id": "run-2", "feature": "second-feature", "status": "needs changes", "stale": true}
    ]}})
}

fn detail() -> Value {
    json!({"status": 200, "json": {"stale": false, "link": null, "summary": ["adds the thing"], "risks": [], "files": ["a.rs"], "tests": ["t"], "rounds": []}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

// The report request is made before the project list because a child's effect runs first.
#[test]
fn the_reviews_tab_lists_the_projects_own_reviews_from_its_own_endpoint() {
    let Some(out) = run("/projects/alpha/reviews", json!([list(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/reviews", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let text = shown(&out["frames"][1]);
    assert!(text.contains("first-feature") && text.contains("second-feature"), "{text}");
    assert!(!text.contains("not available yet"), "{text}");
}

#[test]
fn opening_a_report_stays_under_the_project_and_back_returns_to_its_list() {
    let Some(out) = run("/projects/alpha/reviews", json!([list(), projects(), detail(), list()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"click": "button", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/reviews/run-1", "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/alpha/reviews/run-1", "{out}");
    assert!(shown(&out["frames"][2]).contains("adds the thing"), "{out}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/alpha/reviews", "{out}");
    assert!(shown(&out["frames"][3]).contains("first-feature"), "{out}");
}

#[test]
fn a_report_address_opens_the_report_directly_and_a_malformed_run_is_not_found() {
    let Some(out) = run("/projects/alpha/reviews/run-1", json!([detail(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/reviews/run-1", "{out}");
    assert!(shown(&out["frames"][1]).contains("adds the thing"), "{out}");
    let Some(out) = run("/projects/alpha/reviews/a.b", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).contains("Not found"), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "no report is asked for: {out}");
}

#[test]
fn the_other_sections_are_still_not_built_and_the_global_inbox_is_unchanged() {
    let Some(out) = run("/projects/alpha/orchestrations", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).contains("not available yet"), "{out}");
    let Some(out) = run("/", json!([list()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/reviews", "{out}");
}
