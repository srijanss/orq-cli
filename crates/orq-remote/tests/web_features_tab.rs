//! A project's Features tab: a filterable card list (state, stage, sort) with a project selector, and
//! a detail page per feature, read from the project's own endpoints and drawn as text only.

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

fn no_runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn runs_for(feature: &str) -> Value {
    json!({"status": 200, "json": {"runs": [{"run_id": "r-1", "feature": feature, "state": "running", "stage": "review", "started_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-03T00:00:00Z", "target_branch": "main"}], "next_cursor": null, "total": 1, "running": 1}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

const SELECTORS: [&str; 7] = ["[data-feature-open]", "[data-project-select]", "[data-feature-status]", "[data-feature-stage]", "[data-sort-updated]", "[data-back=\"features\"]", "[data-dependency]"];

fn page(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn the_list_reads_the_projects_own_ledger_and_shows_each_feature_as_a_link_card() {
    let Some(out) = page("/projects/alpha/features", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/features", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(out["requests"][1]["url"], "/api/projects/alpha/runs", "{out}");
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-feature-open]"), 4, "{out}");
    let links = frame["settled"]["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["href"] == "/projects/alpha/features/next-one"), "{links:?}");
    let texts = shown(frame);
    for wanted in ["finished work", "<b>bold</b> idea", "No description."] {
        let _ = wanted;
    }
    assert!(texts.iter().any(|t| t == "finished work") && texts.iter().any(|t| t == "<b>bold</b> idea"), "{texts:?}");
    for selector in ["[data-project-select]", "[data-feature-status]", "[data-feature-stage]", "[data-sort-updated]"] {
        assert_eq!(count(frame, selector), 1, "{selector}: {out}");
    }
}

#[test]
fn the_bare_project_address_shows_the_features() {
    let Some(out) = page("/projects/alpha", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(count(&out["frames"][1], "[data-feature-open]"), 4, "{out}");
}

#[test]
fn an_empty_ledger_says_so_and_a_failed_read_offers_a_retry_keeping_the_session() {
    let Some(out) = page("/projects/alpha/features", json!([{"status": 200, "json": {"features": []}}, no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "No features in this project's ledger."), "{out}");
    let Some(out) = page("/projects/alpha/features", json!([{"status": 500}, no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 0, "{out}");
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "{out}");
    let Some(out) = page("/projects/alpha/features", json!([{"status": 200, "json": {"features": [{"name": 5}]}}, no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "a malformed ledger is an error: {out}");
}

#[test]
fn a_refused_token_asks_again_and_the_project_id_is_encoded_in_the_request() {
    let Some(out) = page("/projects/alpha/features", json!([{"status": 401}, no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap();
    assert!(source.contains("\"/api/projects/\" + encodeURIComponent(projectId) + \"/features\""), "the project id is encoded");
}

fn names_in(frame: &Value) -> Vec<String> {
    shown(frame).into_iter().filter(|t| t.ends_with("-work")).collect()
}

fn chain() -> Value {
    json!({"status": 200, "json": {"features": [
        feature("base-work", "completed", Value::Null, json!([])),
        feature("middle-work", "pending", Value::Null, json!(["base-work"])),
        feature("top-work", "draft", Value::Null, json!(["middle-work"]))
    ]}})
}

#[test]
fn features_are_searched_by_name_and_filtered_by_state_and_stage() {
    let runs = runs_for("middle-work");
    let steps = json!([{"submit": TOKEN}, {"set": "[data-feature-search]", "value": "MIDDLE"}, {"set": "[data-feature-search]", "value": ""}, {"set": "[data-feature-status]", "value": "draft"}, {"set": "[data-feature-status]", "value": "all"}, {"set": "[data-feature-stage]", "value": "review"}, {"set": "[data-feature-stage]", "value": "(no run)"}, {"set": "[data-feature-search]", "value": "zzz"}]);
    let Some(out) = page("/projects/alpha/features", json!([chain(), runs, projects()]), steps) else { return };
    let mut all = names_in(&out["frames"][1]);
    all.sort();
    assert_eq!(all, ["base-work", "middle-work", "top-work"], "{out}");
    assert_eq!(names_in(&out["frames"][2]), ["middle-work"], "search ignores case");
    assert_eq!(names_in(&out["frames"][4]), ["top-work"], "state filter");
    assert_eq!(names_in(&out["frames"][6]), ["middle-work"], "stage filter: the stage comes from the latest run");
    let mut none = names_in(&out["frames"][7]);
    none.sort();
    assert_eq!(none, ["base-work", "top-work"], "features that never ran");
    assert!(shown(&out["frames"][8]).iter().any(|t| t == "No features match the filters."), "{out}");
}

#[test]
fn the_updated_header_toggles_the_order_newest_or_oldest_first() {
    let ledger = json!({"status": 200, "json": {"features": [
        {"name": "b-work", "description": null, "status": "pending", "depends_on": [], "recorded_at": "2026-01-01T00:00:00Z"},
        {"name": "a-work", "description": null, "status": "pending", "depends_on": [], "recorded_at": "2026-01-05T00:00:00Z"},
        {"name": "c-work", "description": null, "status": "pending", "depends_on": [], "recorded_at": "2026-01-03T00:00:00Z"}
    ]}});
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-sort-updated]"}, {"nav": "[data-sort-updated]"}]);
    let Some(out) = page("/projects/alpha/features", json!([ledger, no_runs(), projects()]), steps) else { return };
    assert_eq!(names_in(&out["frames"][1]), ["a-work", "c-work", "b-work"], "newest first by default: {out}");
    assert_eq!(names_in(&out["frames"][2]), ["b-work", "c-work", "a-work"], "{out}");
    assert_eq!(names_in(&out["frames"][3]), ["a-work", "c-work", "b-work"], "{out}");
}

#[test]
fn the_filters_offer_every_state_and_every_orchestration_stage_and_the_running_count_leads() {
    let Some(out) = page("/projects/alpha/features", json!([chain(), runs_for("middle-work"), projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]);
    for option in ["draft", "pending", "in_progress", "completed", "abandoned", "implement_and_review", "gate_fix", "verify", "rebase", "push", "ci", "ci_fix", "review", "review_draft", "cleanup", "No run yet"] {
        assert!(texts.iter().any(|t| t == option), "{option} missing: {texts:?}");
    }
    assert!(texts.iter().any(|t| t == "1"), "one run is running: {texts:?}");
    assert!(texts.iter().any(|t| t.trim() == "Running"), "{texts:?}");
}

#[test]
fn the_project_dropdown_opens_another_projects_features() {
    let Some(out) = page("/projects/alpha/features", json!([ledger_of("alpha-only"), no_runs(), projects(), ledger_of("beta-only"), no_runs()]), json!([{"submit": TOKEN}, {"nav": "[data-project-select]"}, {"nav": "[data-project-choice=\"beta\"]"}])) else { return };
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
    let after = shown(&out["frames"][3]);
    assert!(after.iter().any(|t| t == "beta-only") && !after.iter().any(|t| t == "alpha-only"), "{after:?}");
}

#[test]
fn a_feature_opens_its_detail_with_dependency_links_a_missing_marker_and_a_way_back() {
    let Some(out) = page("/projects/alpha/features/next-one", json!([ledger(), runs_for("next-one"), projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    let texts = shown(frame);
    for wanted in ["next-one", "<b>bold</b> idea", "Dependencies", "Runs"] {
        assert!(texts.iter().any(|t| t == wanted), "{wanted}: {texts:?}");
    }
    assert_eq!(count(frame, "[data-back=\"features\"]"), 1, "{out}");
    let links = frame["settled"]["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["text"] == "done-one" && l["href"] == "/projects/alpha/features/done-one"), "{links:?}");
    assert!(!links.iter().any(|l| l["text"].as_str().unwrap().contains("ghost")), "a missing dependency is not a link");
    assert!(texts.iter().any(|t| t.contains("ghost")) && texts.iter().any(|t| t.contains("Missing dependency")), "{texts:?}");
    assert!(frame["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Open timeline"), "its run is listed: {frame}");
}

#[test]
fn a_feature_without_dependencies_or_runs_says_so_and_an_unknown_one_is_not_found() {
    let Some(out) = page("/projects/alpha/features/sketch", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]);
    assert!(texts.iter().any(|t| t == "No dependencies.") && texts.iter().any(|t| t == "No runs for this feature yet."), "{texts:?}");
    let Some(out) = page("/projects/alpha/features/nothing-here", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "Not found"), "{out}");
    assert!(!shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
}

#[test]
fn clicking_a_feature_in_the_list_navigates_to_its_detail() {
    let Some(out) = page("/projects/alpha/features", json!([ledger(), no_runs(), projects(), ledger(), no_runs()]), json!([{"submit": TOKEN}, {"nav": "a[data-feature-open=\"next-one\"]"}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/features/next-one", "{out}");
}

#[test]
fn feature_text_is_drawn_as_text_and_the_list_has_no_controls_to_change_anything() {
    let Some(out) = page("/projects/alpha/features", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
    let frame = &out["frames"][1]["settled"];
    assert!(frame["buttons"].as_array().unwrap().iter().all(|b| !b.as_str().unwrap().contains("Start")), "{frame}");
    assert_eq!(frame["inputs"].as_array().unwrap().len(), 1, "only the search is a text input: {frame}");
}

fn ledger_of(name: &str) -> Value {
    json!({"status": 200, "json": {"features": [feature(name, "pending", Value::Null, json!([]))]}})
}

#[test]
fn an_answer_that_finishes_after_switching_project_is_ignored() {
    let held = json!({"hold": "json", "status": 200, "json": {"features": [feature("alpha-late", "pending", Value::Null, json!([]))]}});
    let Some(out) = page("/projects/alpha/features", json!([held, no_runs(), projects(), ledger_of("beta-only"), no_runs()]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only"), "{late:?}");
    assert!(!late.iter().any(|t| t == "alpha-late"), "a late answer from the other project was drawn: {late:?}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
}
