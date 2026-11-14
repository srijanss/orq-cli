//! The Features pages show how many features are in progress beside the title, the way the
//! Orchestrations page shows how many runs are running. The count comes from the ledger, not the
//! filters.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const PILL: &str = "[data-in-progress-count]";

fn page(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": [PILL]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

fn ledger(features: &[(&str, &str)]) -> Value {
    let rows: Vec<Value> = features.iter().map(|(name, status)| feature(name, status)).collect();
    json!({"status": 200, "json": {"features": rows}})
}

fn mixed() -> Value {
    ledger(&[("a-done", "completed"), ("b-busy", "in_progress"), ("c-next", "pending"), ("d-busy", "in_progress"), ("e-sketch", "draft")])
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn pills(frame: &Value) -> u64 {
    frame["settled"]["matches"][PILL].as_u64().unwrap()
}

#[test]
fn a_projects_features_page_counts_the_in_progress_features_whatever_the_filters_show() {
    let steps = json!([{"submit": TOKEN}, {"set": "[data-feature-search]", "value": "b-"}, {"set": "[data-feature-status]", "value": "completed"}]);
    let Some(out) = page("/projects/alpha/features", json!([mixed(), projects()]), steps) else { return };
    for frame in 1..=3 {
        let frame = &out["frames"][frame];
        assert_eq!(pills(frame), 1, "{out}");
        assert!(texts(frame).iter().any(|t| t == "2 in progress"), "{:?}", texts(frame));
    }
}

#[test]
fn the_every_project_features_page_counts_the_in_progress_features_of_every_project() {
    let alpha = ledger(&[("a-busy", "in_progress"), ("a-next", "pending")]);
    let beta = ledger(&[("b-busy", "in_progress"), ("b-also", "in_progress"), ("b-done", "completed")]);
    let Some(out) = page("/features", json!([projects(), alpha, beta]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(pills(frame), 1, "{out}");
    assert!(texts(frame).iter().any(|t| t == "3 in progress"), "{:?}", texts(frame));
}

#[test]
fn a_ledger_with_nothing_in_progress_says_zero_and_a_ledger_still_loading_shows_no_pill() {
    let held = json!({"hold": "json", "status": 200, "json": {"features": [feature("a-done", "completed"), feature("b-next", "pending")]}});
    let Some(out) = page("/projects/alpha/features", json!([held, projects()]), json!([{"submit": TOKEN}, {"release": 0}])) else { return };
    assert_eq!(pills(&out["frames"][1]), 0, "no count while the ledger loads: {out}");
    let frame = &out["frames"][2];
    assert_eq!(pills(frame), 1, "{out}");
    assert!(texts(frame).iter().any(|t| t == "0 in progress"), "{:?}", texts(frame));
}

#[test]
fn a_single_features_page_shows_no_pill() {
    let runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}});
    let Some(out) = page("/projects/alpha/features/b-busy", json!([mixed(), runs, projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert!(texts(frame).iter().any(|t| t == "b-busy"), "the feature's page is shown: {:?}", texts(frame));
    assert_eq!(pills(frame), 0, "{out}");
}
