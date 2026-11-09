//! The review list filters by status, risk and freshness and lists the newest review first; the
//! stale or unverified marker stays on every row it applies to.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/", "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [
        {"run_id": "run-1", "feature": "f-old", "status": "approved", "risk": "low", "stale": false, "freshness": "current", "generated_at": "2026-11-01T10:00:00Z"},
        {"run_id": "run-2", "feature": "f-newest", "status": "needs changes", "risk": "high", "stale": true, "freshness": "stale", "generated_at": "2026-11-05T10:00:00Z"},
        {"run_id": "run-3", "feature": "f-middle", "status": "approved", "risk": "high", "stale": false, "freshness": "unverified", "generated_at": "2026-11-03T10:00:00Z"},
        {"run_id": "run-4", "feature": "f-undated", "status": "approved", "risk": "low", "stale": false, "freshness": "current", "generated_at": null}
    ]}})
}

/// The feature names on the rows, in the order shown.
fn rows(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|b| b.as_str()).filter(|b| b.starts_with("f-")).map(String::from).collect()
}

fn texts(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn filtered(filter: &str, value: &str) -> Value {
    let steps = json!([{"submit": TOKEN}, {"set": format!("select[data-filter={filter}]"), "value": value}]);
    let out = run(json!([list()]), steps).unwrap_or(Value::Null);
    out["frames"].as_array().and_then(|f| f.last()).cloned().unwrap_or(Value::Null)
}

#[test]
fn the_newest_review_is_first_and_an_undated_one_is_last() {
    let Some(out) = run(json!([list()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(rows(&out["frames"][1]), ["f-newest", "f-middle", "f-old", "f-undated"], "{out}");
}

#[test]
fn each_filter_narrows_the_list_and_all_shows_everything_again() {
    if !web_deps::ready() {
        return;
    }
    assert_eq!(rows(&filtered("status", "approved")), ["f-middle", "f-old", "f-undated"]);
    assert_eq!(rows(&filtered("risk", "high")), ["f-newest", "f-middle"]);
    assert_eq!(rows(&filtered("freshness", "stale")), ["f-newest"]);
    assert_eq!(rows(&filtered("freshness", "unverified")), ["f-middle"]);
    assert_eq!(rows(&filtered("status", "")).len(), 4);
}

#[test]
fn the_stale_and_unverified_markers_stay_on_their_rows() {
    let Some(out) = run(json!([list()]), json!([{"submit": TOKEN}])) else { return };
    let text = texts(&out["frames"][1]);
    assert!(text.contains("stale, may be out of date"), "{text}");
    assert!(text.contains("unverified, could not be checked"), "{text}");
    // Filtering to the unverified row keeps its marker.
    assert!(texts(&filtered("freshness", "unverified")).contains("unverified, could not be checked"));
}

#[test]
fn a_filter_that_matches_nothing_says_so() {
    if !web_deps::ready() {
        return;
    }
    let steps = json!([{"submit": TOKEN}, {"set": "select[data-filter=status]", "value": "needs changes"}, {"set": "select[data-filter=risk]", "value": "low"}]);
    let out = run(json!([list()]), steps).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(texts(last).contains("No reviews match these filters."), "{out}");
}
