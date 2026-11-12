//! The every-project features list (the Features tab before a project is chosen) offers the same actions as a
//! project's own list, for each project whose server allows them: a mark on its drafts and pending features, the
//! swipe action, and the marked-actions buttons, one set per project, each sending to its own project. A
//! project with remote actions off offers none on its rows. There is no Add feature: it has no project to add to.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = [
    "[data-mark=\"d1\"]",
    "[data-mark=\"p1\"]",
    "[data-mark=\"p2\"]",
    "tr[data-feature-row=\"p1\"].swiped [data-swipe-start=\"p1\"]",
    "tr[data-feature-row=\"p2\"].swiped",
    "[data-promote-marked]",
    "[data-feature-add]",
];

fn project(id: &str) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

fn run(steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let projects = json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}});
    let alpha = json!({"status": 200, "json": {"features": [feature("d1", "draft"), feature("p1", "pending")], "remote_actions": true}});
    let beta = json!({"status": 200, "json": {"features": [feature("p2", "pending")], "remote_actions": false}});
    let promoted = json!({"status": 200, "json": {"feature": "d1"}});
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/features", "responses": [projects.clone(), alpha.clone(), beta.clone(), promoted, alpha, beta], "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn a_project_that_allows_actions_gets_marks_and_the_swipe_and_one_that_does_not_gets_none() {
    let steps = vec![json!({"swipe": "[data-feature-row=\"p1\"]", "dx": -90}), json!({"swipe": "[data-feature-row=\"p2\"]", "dx": -90})];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 1, "{out}");
    assert_eq!(count(&frames[1], SELECTORS[1]), 1, "{out}");
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "beta has actions off: {out}");
    assert_eq!(count(&frames[1], SELECTORS[6]), 0, "no project to add to: {out}");
    assert_eq!(count(&frames[2], SELECTORS[3]), 1, "{out}");
    assert_eq!(count(&frames[3], SELECTORS[4]), 0, "beta's row does not swipe: {out}");
}

#[test]
fn a_marked_draft_is_promoted_in_its_own_project() {
    let steps = vec![json!({"click": "[data-mark=\"d1\"]"}), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[5]), 1, "{out}");
    let texts: Vec<&str> = frames[3]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("in alpha")), "says which project: {texts:?}");
    let posts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/features/d1/promote");
}
