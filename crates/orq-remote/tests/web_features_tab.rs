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

const SELECTORS: [&str; 5] = ["[data-feature-open]", "[data-project-select]", "[data-feature-status]", "[data-back=\"features\"]", "[data-dependency]"];

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
fn the_list_reads_the_projects_own_ledger_and_shows_each_feature_as_a_link_row_with_its_state() {
    let Some(out) = page("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/features", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(!out["requests"].as_array().unwrap().iter().any(|r| r["url"].as_str().unwrap().ends_with("/runs")), "the list needs no runs: {out}");
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-feature-open]"), 4, "{out}");
    let links = frame["settled"]["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["href"] == "/projects/alpha/features/next-one"), "{links:?}");
    let texts = shown(frame);
    assert!(!texts.iter().any(|t| t == "finished work" || t == "<b>bold</b> idea"), "descriptions belong to the detail page: {texts:?}");
    for selector in ["[data-project-select]", "[data-feature-status]"] {
        assert_eq!(count(frame, selector), 1, "{selector}: {out}");
    }
    for gone in ["Stage", "Updated", "Running"] {
        assert!(!texts.iter().any(|t| t == gone), "{gone} is not shown: {texts:?}");
    }
}

#[test]
fn in_progress_and_draft_counts_lead_the_list_with_their_state_colours() {
    let ledger = json!({"status": 200, "json": {"features": [
        feature("a-work", "in_progress", Value::Null, json!([])),
        feature("b-work", "in_progress", Value::Null, json!([])),
        feature("c-work", "draft", Value::Null, json!([])),
        feature("d-work", "pending", Value::Null, json!([]))
    ]}});
    let Some(out) = page("/projects/alpha/features", json!([ledger, projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1]["settled"];
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().position(|t| t.trim() == needle).unwrap_or_else(|| panic!("{needle}: {texts:?}"));
    assert!(at("In progress") < at("Draft") && at("Draft") < at("Feature"), "the counts lead the list: {texts:?}");
    assert!(texts.iter().any(|t| t.trim() == "2") && texts.iter().any(|t| t.trim() == "1"), "{texts:?}");
    let classes = frame["classes"].as_array().unwrap();
    assert!(classes.iter().any(|c| c == "state-in_progress") && classes.iter().any(|c| c == "state-draft"), "{classes:?}");
}

#[test]
fn the_bare_project_address_shows_the_features() {
    let Some(out) = page("/projects/alpha", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(count(&out["frames"][1], "[data-feature-open]"), 4, "{out}");
}

#[test]
fn an_empty_ledger_says_so_and_a_failed_read_offers_a_retry_keeping_the_session() {
    let Some(out) = page("/projects/alpha/features", json!([{"status": 200, "json": {"features": []}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "No features in this project's ledger."), "{out}");
    let Some(out) = page("/projects/alpha/features", json!([{"status": 500}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 0, "{out}");
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "{out}");
    let Some(out) = page("/projects/alpha/features", json!([{"status": 200, "json": {"features": [{"name": 5}]}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "a malformed ledger is an error: {out}");
}

#[test]
fn a_refused_token_asks_again_and_the_project_id_is_encoded_in_the_request() {
    let Some(out) = page("/projects/alpha/features", json!([{"status": 401}, projects()]), json!([{"submit": TOKEN}])) else { return };
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
fn features_are_searched_by_name_and_filtered_by_state_which_offers_every_state() {
    let steps = json!([{"submit": TOKEN}, {"set": "[data-feature-search]", "value": "MIDDLE"}, {"set": "[data-feature-search]", "value": ""}, {"set": "[data-feature-status]", "value": "draft"}, {"set": "[data-feature-status]", "value": "all"}, {"set": "[data-feature-search]", "value": "zzz"}]);
    let Some(out) = page("/projects/alpha/features", json!([chain(), projects()]), steps) else { return };
    assert_eq!(names_in(&out["frames"][1]), ["base-work", "middle-work", "top-work"], "{out}");
    assert_eq!(names_in(&out["frames"][2]), ["middle-work"], "search ignores case");
    assert_eq!(names_in(&out["frames"][4]), ["top-work"], "state filter");
    assert_eq!(names_in(&out["frames"][5]).len(), 3, "{out}");
    assert!(shown(&out["frames"][6]).iter().any(|t| t == "No features match the filters."), "{out}");
    let texts = shown(&out["frames"][1]);
    for option in ["draft", "pending", "in_progress", "completed", "abandoned"] {
        assert!(texts.iter().any(|t| t == option), "{option} missing: {texts:?}");
    }
}

#[test]
fn the_default_features_page_lists_every_projects_features_in_one_table_with_a_project_column() {
    let alpha = json!({"status": 200, "json": {"features": [feature("alpha-work", "pending", Value::Null, json!([]))]}});
    let beta = json!({"status": 200, "json": {"features": [feature("beta-work", "draft", Value::Null, json!([]))]}});
    let Some(out) = page("/features", json!([projects(), alpha, beta]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-project-select]"), 1, "{out}");
    assert_eq!(count(frame, "[data-feature-open]"), 2, "{out}");
    assert_eq!(names_in(frame), ["alpha-work", "beta-work"], "{out}");
    let texts = shown(frame);
    assert!(!texts.iter().any(|t| t == "Project"), "no Project column, same table as a project: {texts:?}");
    let links = frame["settled"]["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["href"] == "/projects/beta/features/beta-work"), "{links:?}");
    assert!(texts.iter().any(|t| t == "All projects"), "all projects is chosen first: {texts:?}");
}

#[test]
fn choosing_a_project_from_the_default_page_filters_to_it_and_all_projects_returns() {
    let alpha = json!({"status": 200, "json": {"features": [feature("alpha-work", "pending", Value::Null, json!([]))]}});
    let beta = json!({"status": 200, "json": {"features": [feature("beta-work", "draft", Value::Null, json!([]))]}});
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-project-select]"}, {"nav": "[data-project-choice=\"beta\"]"}]);
    let Some(out) = page("/features", json!([projects(), alpha, beta, ledger_of("beta-work")]), steps) else { return };
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
    assert_eq!(names_in(&out["frames"][3]), ["beta-work"], "{out}");
}

#[test]
fn the_project_dropdown_opens_another_projects_features() {
    let Some(out) = page("/projects/alpha/features", json!([ledger_of("alpha-only"), projects(), ledger_of("beta-only")]), json!([{"submit": TOKEN}, {"nav": "[data-project-select]"}, {"nav": "[data-project-choice=\"beta\"]"}])) else { return };
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
    let after = shown(&out["frames"][3]);
    assert!(after.iter().any(|t| t == "beta-only") && !after.iter().any(|t| t == "alpha-only"), "{after:?}");
}

#[test]
fn a_feature_opens_its_detail_with_dependency_links_a_missing_marker_and_a_way_back() {
    let Some(out) = page("/projects/alpha/features/next-one", json!([ledger(), runs_for("next-one"), projects()]), json!([{"submit": TOKEN}, {"nav": "[data-feature-tab=\"dependencies\"]"}])) else { return };
    let frame = &out["frames"][2];
    let texts = shown(frame);
    for wanted in ["next-one", "Dependencies", "Runs"] {
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
    let Some(out) = page("/projects/alpha/features/sketch", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}, {"nav": "[data-feature-tab=\"dependencies\"]"}])) else { return };
    let texts = shown(&out["frames"][2]);
    assert!(texts.iter().any(|t| t == "No dependencies.") && texts.iter().any(|t| t == "No runs for this feature yet."), "{texts:?}");
    let Some(out) = page("/projects/alpha/features/nothing-here", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "Not found"), "{out}");
    assert!(!shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
}

#[test]
fn clicking_a_feature_in_the_list_navigates_to_its_detail() {
    let Some(out) = page("/projects/alpha/features", json!([ledger(), projects(), ledger()]), json!([{"submit": TOKEN}, {"nav": "a[data-feature-open=\"next-one\"]"}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/features/next-one", "{out}");
}

#[test]
fn the_list_has_no_controls_to_change_anything_and_a_description_is_drawn_as_text_on_the_detail_page() {
    let Some(out) = page("/projects/alpha/features/next-one", json!([ledger(), runs_for("next-one"), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
    let Some(out) = page("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
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
    let Some(out) = page("/projects/alpha/features", json!([held, projects(), ledger_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only"), "{late:?}");
    assert!(!late.iter().any(|t| t == "alpha-late"), "a late answer from the other project was drawn: {late:?}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
}

#[test]
fn a_long_list_is_paged_ten_at_a_time_and_a_filter_returns_to_the_first_page() {
    let many = json!({"status": 200, "json": {"features": (1..=23).map(|n| feature(&format!("f{n:02}-work"), "pending", Value::Null, json!([]))).collect::<Vec<_>>()}});
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-page-next]"}, {"nav": "[data-page-next]"}, {"nav": "[data-page-prev]"}, {"nav": "[data-page-next]"}, {"set": "[data-feature-search]", "value": "f1"}]);
    let Some(out) = page("/projects/alpha/features", json!([many, projects()]), steps) else { return };
    let names = |i: usize| names_in(&out["frames"][i]);
    assert_eq!((names(1).len(), names(1)[0].as_str(), names(1)[9].as_str()), (10, "f01-work", "f10-work"), "{out}");
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "Page 1 of 3"), "{out}");
    assert_eq!((names(2).len(), names(2)[0].as_str()), (10, "f11-work"), "{out}");
    assert_eq!((names(3).len(), names(3)[0].as_str()), (3, "f21-work"), "the last page is the remainder: {out}");
    assert_eq!(names(4)[0], "f11-work", "{out}");
    assert_eq!(names(5)[0], "f21-work", "{out}");
    let filtered = names(6);
    assert_eq!((filtered.len(), filtered[0].as_str()), (10, "f10-work"), "a new search starts on page 1: {out}");
    let few = shown(&out["frames"][6]);
    assert!(few.iter().any(|t| t == "Page 1 of 1") || !few.iter().any(|t| t.starts_with("Page ")), "{few:?}");
}

#[test]
fn a_short_list_has_no_pager() {
    let Some(out) = page("/projects/alpha/features", json!([chain(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(!shown(&out["frames"][1]).iter().any(|t| t.starts_with("Page ")), "{out}");
}

#[test]
fn a_feature_detail_has_description_test_file_and_dependencies_tabs_starting_on_the_description() {
    let mut with_test = feature("next-one", "pending", json!("an idea"), json!(["done-one"]));
    with_test["test_file"] = json!("-p orq-remote --test next");
    let ledger = json!({"status": 200, "json": {"features": [feature("done-one", "completed", Value::Null, json!([])), with_test]}});
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-feature-tab=\"test\"]"}, {"nav": "[data-feature-tab=\"dependencies\"]"}, {"nav": "[data-feature-tab=\"description\"]"}]);
    let Some(out) = page("/projects/alpha/features/next-one", json!([ledger, runs_for("next-one"), projects()]), steps) else { return };
    let has = |i: usize, t: &str| shown(&out["frames"][i]).iter().any(|x| x == t);
    assert!(["Description", "Test file", "Dependencies"].iter().all(|t| has(1, t)), "{out}");
    assert!(has(1, "an idea") && !has(1, "-p orq-remote --test next") && !has(1, "done-one"), "{out}");
    assert!(has(2, "-p orq-remote --test next") && !has(2, "an idea"), "{out}");
    assert!(has(3, "done-one") && !has(3, "an idea"), "{out}");
    assert!(has(4, "an idea") && !has(4, "done-one"), "{out}");
    assert!(shown(&out["frames"][1]).iter().all(|t| t != "No test file recorded."), "{out}");
}

#[test]
fn a_feature_without_a_test_file_says_so() {
    let Some(out) = page("/projects/alpha/features/sketch", json!([ledger(), no_runs(), projects()]), json!([{"submit": TOKEN}, {"nav": "[data-feature-tab=\"test\"]"}])) else { return };
    assert!(shown(&out["frames"][2]).iter().any(|t| t == "No test file recorded."), "{out}");
}
