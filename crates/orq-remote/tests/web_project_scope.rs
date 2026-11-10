//! The chosen project lives in the page address; the sidebar shows it with a switcher, and one
//! project's page is never drawn with another project's data.

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

fn two() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn inside_a_project_the_sidebar_lists_the_projects_with_the_current_one_chosen_and_its_sections() {
    let Some(out) = run("/projects/alpha/timeline", json!([two()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let frame = &out["frames"][1]["settled"];
    assert_eq!(frame["switchers"], json!([{"value": "alpha", "options": ["", "alpha", "beta"]}]), "{out}");
    assert_eq!(frame["sidebarLinks"], json!(["/dashboard", "/projects/alpha/features", "/projects/alpha/orchestrations", "/projects/alpha/reviews", "/settings"]), "{out}");
}

#[test]
fn switching_project_moves_the_address_and_the_sections_to_the_other_project_keeping_the_section() {
    let Some(out) = run("/projects/alpha/timeline", json!([two()]), json!([{"submit": TOKEN}, {"select": "beta"}])) else { return };
    let frame = &out["frames"][2]["settled"];
    assert_eq!(frame["path"], "/projects/beta/timeline", "{out}");
    assert_eq!(frame["switchers"][0]["value"], "beta", "{out}");
    assert!(frame["sidebarLinks"].as_array().unwrap().iter().all(|l| !l.as_str().unwrap().contains("alpha")), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "the list is not fetched again: {out}");
}

#[test]
fn an_unknown_project_and_one_the_caller_may_not_see_look_the_same_as_not_found() {
    let Some(out) = run("/projects/zzz/timeline", json!([two()]), json!([{"submit": TOKEN}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Not found"), "{text}");
    assert!(!text.contains("not available yet") && !text.contains("zzz"), "{text}");
}

#[test]
fn leaving_the_project_cancels_the_list_request_still_in_flight() {
    let Some(out) = run("/projects/alpha/timeline", json!([{"hold": "fetch", "status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}, {"go": "/projects"}])) else { return };
    assert_eq!(out["requests"][0]["aborted"], true, "{out}");
}

#[test]
fn a_project_list_that_cannot_be_read_does_not_block_the_page() {
    for reply in [json!({"reject": true}), json!({"status": 503}), json!({"status": 200, "json": {"projects": "x"}})] {
        let Some(out) = run("/projects/alpha/timeline", json!([reply.clone()]), json!([{"submit": TOKEN}])) else { return };
        let frame = &out["frames"][1];
        assert!(shown(frame).contains("not available yet"), "{reply}: {out}");
        assert_eq!(frame["settled"]["switchers"], json!([{"value": "alpha", "options": ["", "alpha"]}]), "{reply}");
        assert!(frame["settled"]["inputs"].as_array().unwrap().is_empty(), "{reply}: still signed in");
    }
}

#[test]
fn a_refused_token_on_the_project_list_asks_for_the_token_again() {
    let Some(out) = run("/projects/alpha/timeline", json!([{"status": 401}]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
}

#[test]
fn coming_back_into_a_project_does_not_judge_it_by_the_list_from_the_last_visit() {
    let Some(out) = run("/projects/alpha/timeline", json!([two(), {"hold": "fetch", "status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}, {"go": "/projects"}, {"go": "/projects/gamma/timeline"}])) else { return };
    let text = shown(&out["frames"][3]);
    assert!(!text.contains("Not found"), "an old list decided a new visit: {text}");
    assert_eq!(out["frames"][3]["settled"]["switchers"], json!([{"value": "gamma", "options": ["", "gamma"]}]), "{out}");
}

