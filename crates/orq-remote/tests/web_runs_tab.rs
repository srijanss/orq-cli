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
    assert!(texts.iter().any(|t| t == "implement"), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("run-b")) && out["frames"][1]["settled"]["times"].as_array().unwrap().iter().any(|m| m["datetime"] == "2026-11-02T10:30:00Z"), "{texts:?}");
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

fn runs_of(feature: &str) -> Value {
    json!({"status": 200, "json": {"runs": [row("run-x", feature, "running", Value::Null)]}})
}

#[test]
fn switching_project_never_shows_the_other_projects_runs_loaded_or_late() {
    // Loaded: alpha's rows are gone once beta is chosen, and beta's own endpoint is read.
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs_of("alpha-only"), projects(), runs_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "alpha-only"), "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/beta/orchestrations", "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/beta/runs", "{out}");
    let after = shown(&out["frames"][2]);
    assert!(after.iter().any(|t| t == "beta-only") && !after.iter().any(|t| t == "alpha-only"), "{after:?}");

    // Late: alpha's request is still waiting when beta is chosen, and its answer changes nothing.
    let held = json!({"hold": "fetch", "status": 200, "json": {"runs": [row("run-late", "alpha-late", "running", Value::Null)]}});
    let Some(out) = run("/projects/alpha/orchestrations", json!([held, projects(), runs_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    assert_eq!(out["requests"][0]["aborted"], true, "{out}");
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only") && !late.iter().any(|t| t == "alpha-late"), "{late:?}");

    // Finishing late: the status arrived, the body is held, and it completes after the switch.
    let held = json!({"hold": "json", "status": 200, "json": {"runs": [row("run-late", "alpha-late", "running", Value::Null)]}});
    let Some(out) = run("/projects/alpha/orchestrations", json!([held, projects(), runs_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only") && !late.iter().any(|t| t == "alpha-late"), "{late:?}");
}

#[test]
fn while_the_other_projects_runs_are_loading_the_loaded_rows_are_already_gone() {
    let held = json!({"hold": "fetch", "status": 200, "json": {"runs": [row("run-b", "beta-only", "running", Value::Null)]}});
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs_of("alpha-only"), projects(), held]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 2}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "alpha-only"), "{out}");
    // Beta's request is made and still waiting: alpha's rows must not stand under beta's address.
    assert_eq!(out["requests"][2]["url"], "/api/projects/beta/runs", "{out}");
    let waiting = shown(&out["frames"][2]);
    assert!(!waiting.iter().any(|t| t == "alpha-only") && !waiting.iter().any(|t| t == "beta-only"), "{waiting:?}");
    let done = shown(&out["frames"][3]);
    assert!(done.iter().any(|t| t == "beta-only") && !done.iter().any(|t| t == "alpha-only"), "{done:?}");
}

#[test]
fn the_list_is_a_table_under_the_header_status_and_project_picker_narrowed_by_search_state_and_stage() {
    let steps = json!([{"submit": TOKEN}, {"set": "select[data-run-state]", "value": "running"}, {"set": "select[data-run-state]", "value": "all"}, {"set": "select[data-run-stage]", "value": "implement"}, {"set": "select[data-run-stage]", "value": "all"}, {"set": "input[data-run-search]", "value": "older"}]);
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs(), projects()]), steps) else { return };
    let first = shown(&out["frames"][1]);
    assert!(first.iter().any(|t| t == "Connected") && first.iter().any(|t| t.starts_with("Last updated")), "the status sits in the header row: {first:?}");
    assert!(first.iter().any(|t| t == "Project:") && first.iter().any(|t| t == "1 running"), "{first:?}");
    assert!(!first.iter().any(|t| t.starts_with("Last checked")), "no footer under the table: {first:?}");
    let rows = |frame: usize| -> Vec<String> { shown(&out["frames"][frame]).into_iter().filter(|t| t.ends_with("feature")).collect() };
    assert_eq!(rows(1), ["<b>bold</b> feature", "older-feature", "odd-feature"]);
    assert_eq!(rows(2), ["<b>bold</b> feature"], "state: running");
    assert_eq!(rows(4), ["<b>bold</b> feature"], "stage: implement");
    assert_eq!(rows(6), ["older-feature"], "search");
    assert!(shown(&out["frames"][6]).iter().any(|t| t == "1"), "the count follows the filters: {:?}", shown(&out["frames"][6]));
}

/// A completed run that was delivered without a review shows how it ended; a hand-merged run is
/// still shown as Merged.
#[test]
fn a_finished_run_that_was_pushed_or_merged_locally_shows_that_ending() {
    let with_ending = |id: &str, feature: &str, ending: Value| {
        let mut r = row(id, feature, "completed", Value::Null);
        r["delivery"] = json!("push");
        r["ending"] = ending;
        r
    };
    let runs = json!({"status": 200, "json": {"runs": [
        with_ending("run-p", "pushed-feature", json!("pushed")),
        with_ending("run-m", "merged-feature", json!("merged")),
        with_ending("run-r", "reviewed-feature", Value::Null),
        row("run-h", "hand-merged-feature", "merged", Value::Null)
    ]}});
    let Some(out) = run("/projects/alpha/orchestrations", json!([runs, projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]);
    assert!(texts.iter().any(|t| t == "Pushed"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Merged locally"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Completed"), "a run with no delivery ending stays Completed: {texts:?}");
    assert!(texts.iter().any(|t| t == "Merged"), "{texts:?}");
}
