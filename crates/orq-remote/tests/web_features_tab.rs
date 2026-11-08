//! A project's Features tab: the ledger features grouped by status, with their dependencies, read
//! from the project's own endpoint and drawn as text only.

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

fn feature(name: &str, status: &str, description: Value, depends_on: Value) -> Value {
    json!({"name": name, "description": description, "status": status, "depends_on": depends_on, "recorded_at": null})
}

fn ledger() -> Value {
    json!({"status": 200, "json": {"features": [
        feature("done-one", "completed", json!("finished work"), json!([])),
        feature("next-one", "pending", json!("<b>bold</b> idea"), json!(["done-one", "ghost"])),
        feature("sketch", "draft", Value::Null, json!([])),
        feature("strange", "waiting", Value::Null, json!([]))
    ]}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

// The ledger request is made before the project list because a child's effect runs first.
#[test]
fn the_features_tab_reads_the_projects_own_ledger_and_groups_features_by_status() {
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/features", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().position(|t| t == needle).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    // Draft, pending and completed groups come in that order, and a status the page does not know comes last.
    assert!(at("draft") < at("pending") && at("pending") < at("completed") && at("completed") < at("waiting"), "{texts:?}");
    assert!(at("sketch") < at("next-one") && at("next-one") < at("done-one") && at("done-one") < at("strange"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "finished work"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("not available yet")), "{texts:?}");
}

#[test]
fn dependencies_are_listed_and_one_missing_from_the_ledger_is_marked() {
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]).join(" | ");
    assert!(texts.contains("Depends on: done-one, ghost (not in this ledger)"), "{texts}");
}

#[test]
fn feature_text_is_drawn_as_text_and_there_are_no_controls_to_change_anything() {
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
    let frame = &out["frames"][1]["settled"];
    assert!(frame["buttons"].as_array().unwrap().is_empty(), "{frame}");
}

#[test]
fn an_empty_ledger_says_so_and_a_failed_read_offers_a_retry_keeping_the_session() {
    let Some(out) = run("/projects/alpha/features", json!([{"status": 200, "json": {"features": []}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "No features in this project's ledger."), "{out}");
    let Some(out) = run("/projects/alpha/features", json!([{"status": 500}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 0, "{out}");
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "{out}");
    let Some(out) = run("/projects/alpha/features", json!([{"status": 200, "json": {"features": [{"name": 5}]}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "a malformed ledger is an error: {out}");
}

#[test]
fn a_refused_token_asks_again_and_the_project_id_is_encoded_in_the_request() {
    let Some(out) = run("/projects/alpha/features", json!([{"status": 401}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap();
    assert!(source.contains("\"/api/projects/\" + encodeURIComponent(projectId) + \"/features\""), "the project id is encoded");
}
