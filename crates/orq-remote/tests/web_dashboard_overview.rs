//! The Dashboard overview: a row of totals above a "Projects" section, and a connection status.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "responses": responses, "steps": [{"submit": TOKEN}]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn project(id: &str, active: u64, waiting: u64) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": active, "reviews_waiting": waiting, "failed_runs": 0, "failed_window_hours": 24})
}

fn texts(out: &Value) -> Vec<String> {
    out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn value_of(texts: &[String], label: &str) -> String {
    let at = texts.iter().position(|t| t == label).unwrap_or_else(|| panic!("{label} missing: {texts:?}"));
    texts[at - 1].clone()
}

#[test]
fn totals_sum_the_readable_projects_and_count_every_project() {
    let broken = json!({"id": "broken", "name": "broken", "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": 24});
    let Some(out) = run(json!([{"status": 200, "json": {"projects": [project("alpha", 2, 1), project("beta", 3, 4), broken]}}])) else { return };
    let texts = texts(&out);
    assert_eq!(value_of(&texts, "Projects"), "3", "{texts:?}");
    assert_eq!(value_of(&texts, "Active runs"), "5", "{texts:?}");
    assert_eq!(value_of(&texts, "Reviews waiting"), "5", "{texts:?}");
    assert!(texts.iter().any(|t| t == "Last updated"), "{texts:?}");
}

#[test]
fn the_project_cards_sit_under_a_projects_heading_and_the_page_says_it_is_connected() {
    let Some(out) = run(json!([{"status": 200, "json": {"projects": [project("alpha", 0, 0)]}}])) else { return };
    let texts = texts(&out);
    let heading = texts.iter().position(|t| t == "Projects").and_then(|first| texts.iter().skip(first + 1).position(|t| t == "Projects")).map(|n| n + 1);
    assert!(heading.is_some(), "a Projects section heading besides the total's label: {texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("Connected")), "{texts:?}");
}
