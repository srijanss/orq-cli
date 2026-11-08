//! A project's Orchestrations tab: that project's runs with state and stage, read from the
//! project's own endpoint and drawn as text only.

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

fn row(id: &str, feature: &str, state: &str, stage: Value) -> Value {
    json!({"run_id": id, "feature": feature, "target_branch": "main", "state": state, "stage": stage, "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [
        row("run-b", "<b>bold</b> feature", "running", json!("implement")),
        row("run-a", "older-feature", "needs_review", Value::Null),
        row("run-c", "odd-feature", "teleporting", Value::Null)
    ]}})
}

// The runs request is made before the project list because a child's effect runs first.
#[test]
fn the_orchestrations_tab_reads_the_projects_own_runs_and_shows_state_stage_and_times() {
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/runs", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    // The server's order (newest first) is kept.
    assert!(at("bold") < at("older-feature") && at("older-feature") < at("odd-feature"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "<b>bold</b> feature"), "markup is shown literally: {texts:?}");
    assert!(texts.iter().any(|t| t == "Running") && texts.iter().any(|t| t == "Needs review"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Stage: implement"), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("run-b")) && texts.iter().any(|t| t.contains("2026-11-02T10:30:00Z")), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("not available yet")), "{texts:?}");
}

#[test]
fn a_state_the_page_does_not_know_is_shown_as_unknown_never_as_a_success() {
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]);
    assert!(texts.iter().any(|t| t == "Unknown"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("teleporting")), "{texts:?}");
}

#[test]
fn a_project_with_no_runs_says_so_and_a_malformed_answer_draws_no_rows() {
    let Some(out) = run("/projects/alpha/orchestrations", json!([{"status": 200, "json": {"runs": []}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "No runs in this project yet."), "{out}");
    let Some(out) = run("/projects/alpha/orchestrations", json!([{"status": 200, "json": {"runs": [{"run_id": 5}]}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]);
    assert!(!texts.iter().any(|t| t == "No runs in this project yet.") && !texts.iter().any(|t| t == "Unknown"), "{texts:?}");
}
