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
    assert!(!texts.contains("ghost"), "dependencies stay folded until asked for: {texts}");
    assert_eq!(out["frames"][1]["settled"]["buttons"], json!(["Dependencies (2)"]), "{out}");
}

#[test]
fn an_expanded_feature_lists_its_dependencies_as_links_and_marks_a_missing_one() {
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    let frame = &out["frames"][2]["settled"];
    let links = frame["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{frame}");
    assert_eq!((&links[0]["text"], &links[0]["href"]), (&json!("done-one"), &json!("/projects/alpha/features")), "{frame}");
    let texts = shown(&out["frames"][2]);
    let at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("ghost") > 0 && texts.iter().any(|t| t.contains("Missing dependency")), "{texts:?}");
    assert!(!links.iter().any(|l| l["text"].as_str().unwrap().contains("ghost")), "a missing dependency is not a link");
    // Folding it again hides them.
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"click": "button", "index": 0}])) else { return };
    assert!(!shown(&out["frames"][3]).join(" | ").contains("ghost"), "{out}");
}

fn chain() -> Value {
    json!({"status": 200, "json": {"features": [
        feature("base-work", "completed", Value::Null, json!([])),
        feature("middle-work", "pending", Value::Null, json!(["base-work"])),
        feature("top-work", "draft", Value::Null, json!(["middle-work"]))
    ]}})
}

#[test]
fn a_dependency_link_opens_its_feature_even_when_a_filter_hides_it() {
    let steps = json!([{"submit": TOKEN}, {"set": "[data-feature-search]", "value": "top"}, {"click": "button", "index": 0}, {"nav": "a[data-dependency=\"middle-work\"]"}]);
    let Some(out) = run("/projects/alpha/features", json!([chain(), projects()]), steps) else { return };
    let filtered = shown(&out["frames"][2]);
    assert!(filtered.iter().any(|t| t == "top-work") && !filtered.iter().any(|t| t == "middle-work"), "{filtered:?}");
    let after = &out["frames"][4];
    let texts = shown(after);
    assert!(texts.iter().any(|t| t == "middle-work") && texts.iter().any(|t| t == "base-work"), "the filter was cleared to show the target: {texts:?}");
    assert_eq!(after["settled"]["inputs"][0]["value"], "", "{after}");
    // The target is opened too, so its own dependency shows as a link.
    assert!(after["settled"]["links"].as_array().unwrap().iter().any(|l| l["text"] == "base-work"), "{after}");
    assert_eq!(after["settled"]["path"], "/projects/alpha/features", "no new address is made up");
    // The target card is scrolled into view and takes the focus, so the reader is not left at the source.
    assert_eq!(after["settled"]["scrolled"], json!(["feature-middle-work"]), "{after}");
    assert_eq!(after["settled"]["focused"], "feature-middle-work", "{after}");
    assert_eq!(out["frames"][3]["settled"]["scrolled"], json!([]), "nothing scrolls before the link is followed");
}

#[test]
fn features_are_searched_by_name_and_filtered_by_status() {
    let Some(out) = run("/projects/alpha/features", json!([chain(), projects()]), json!([{"submit": TOKEN}, {"set": "[data-feature-search]", "value": "MIDDLE"}, {"set": "[data-feature-status]", "value": "draft"}, {"set": "[data-feature-search]", "value": ""}, {"set": "[data-feature-status]", "value": "all"}])) else { return };
    let names = |i: usize| shown(&out["frames"][i]).into_iter().filter(|t| t.ends_with("-work")).collect::<Vec<_>>();
    assert_eq!(names(1), ["top-work", "middle-work", "base-work"]);
    assert_eq!(names(2), ["middle-work"], "search ignores case and hides the other groups");
    // Each status is one option in the filter, and a group heading would be a second mention.
    for status in ["draft", "completed"] {
        assert_eq!(shown(&out["frames"][2]).iter().filter(|t| *t == status).count(), 1, "the {status} group is hidden");
        assert_eq!(shown(&out["frames"][1]).iter().filter(|t| *t == status).count(), 2, "the {status} group is shown");
    }
    assert!(shown(&out["frames"][3]).iter().any(|t| t == "No features match the filters."), "search and status together");
    assert_eq!(names(4), ["top-work"]);
    assert_eq!(names(5), ["top-work", "middle-work", "base-work"]);
    let options = out["frames"][1]["settled"]["texts"].as_array().unwrap();
    assert!(options.iter().any(|t| t == "All statuses"), "{options:?}");
}

#[test]
fn feature_text_is_drawn_as_text_and_there_are_no_controls_to_change_anything() {
    let Some(out) = run("/projects/alpha/features", json!([ledger(), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "<b>bold</b> idea"), "{out}");
    let frame = &out["frames"][1]["settled"];
    // The only buttons fold and unfold dependencies; the only inputs are the search and status filter.
    assert!(frame["buttons"].as_array().unwrap().iter().all(|b| b.as_str().unwrap().starts_with("Dependencies (")), "{frame}");
    assert_eq!(frame["inputs"].as_array().unwrap().len(), 1, "{frame}");
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

fn ledger_of(name: &str) -> Value {
    json!({"status": 200, "json": {"features": [feature(name, "pending", Value::Null, json!([]))]}})
}

#[test]
fn switching_project_never_shows_the_other_projects_features_loaded_or_late() {
    // Loaded: alpha's ledger is gone once beta is chosen, and beta's own endpoint is read.
    let Some(out) = run("/projects/alpha/features", json!([ledger_of("alpha-only"), projects(), ledger_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|t| t == "alpha-only"), "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/beta/features", "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/beta/features", "{out}");
    let after = shown(&out["frames"][2]);
    assert!(after.iter().any(|t| t == "beta-only") && !after.iter().any(|t| t == "alpha-only"), "{after:?}");

    // Late: alpha's answer is still waiting when beta is chosen, and arriving afterwards changes nothing.
    let held = json!({"hold": "fetch", "status": 200, "json": {"features": [feature("alpha-late", "pending", Value::Null, json!([]))]}});
    let Some(out) = run("/projects/alpha/features", json!([held, projects(), ledger_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    assert_eq!(out["requests"][0]["aborted"], true, "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/beta/features", "{out}");
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only") && !late.iter().any(|t| t == "alpha-late"), "{late:?}");
}

#[test]
fn an_answer_that_finishes_after_switching_project_is_ignored() {
    // The status arrives, then the body is held: aborting does not reject it, so it really does finish late.
    let held = json!({"hold": "json", "status": 200, "json": {"features": [feature("alpha-late", "pending", Value::Null, json!([]))]}});
    let Some(out) = run("/projects/alpha/features", json!([held, projects(), ledger_of("beta-only")]), json!([{"submit": TOKEN}, {"select": "beta"}, {"release": 0}])) else { return };
    assert_eq!(out["requests"][2]["url"], "/api/projects/beta/features", "{out}");
    let late = shown(&out["frames"][3]);
    assert!(late.iter().any(|t| t == "beta-only"), "{late:?}");
    assert!(!late.iter().any(|t| t == "alpha-late"), "a late answer from the other project was drawn: {late:?}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/beta/features", "{out}");
}
