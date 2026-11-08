//! The states every page can be in besides "ready": loading, disconnected, failed, not found.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "alpha", "status": "approved", "stale": false}]}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn a_pending_page_shows_a_skeleton_that_still_says_what_is_loading() {
    let Some(out) = run(json!([{"hold": "fetch", "status": 200, "json": {"reviews": []}}]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(frame["settled"]["skeletons"], 1, "{out}");
    assert!(shown(frame).contains("Loading the review inbox"), "{out}");
}

#[test]
fn an_unreachable_server_shows_a_disconnected_banner_with_the_time_and_keeps_the_token_for_a_retry() {
    let Some(out) = run(json!([{"reject": true}, list()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    let first = &out["frames"][1];
    let text = shown(first);
    assert!(text.contains("Disconnected"), "{text}");
    let checked = text.split("Last checked ").nth(1).unwrap_or_else(|| panic!("no last-checked time: {text}"));
    assert!(checked.chars().next().unwrap().is_ascii_digit(), "{text}");
    assert!(first["settled"]["inputs"].as_array().unwrap().is_empty(), "the token is kept, not asked for again: {out}");
    assert!(!first["settled"].to_string().contains(TOKEN));
    let after = &out["frames"][2];
    assert!(shown(after).contains("alpha") && !shown(after).contains("Disconnected"), "retry recovers: {out}");
    assert_eq!(out["requests"][1]["headers"]["Authorization"], format!("Bearer {TOKEN}"), "the retry reuses the token held in memory");
}

#[test]
fn a_failed_refresh_keeps_the_same_data_marked_outdated_beside_a_retry() {
    for failure in [json!({"status": 503}), json!({"reject": true})] {
        let Some(out) = run(json!([list(), failure.clone()]), json!([{"submit": TOKEN}, {"nav": "[data-refresh]"}])) else { return };
        let text = shown(&out["frames"][2]);
        assert!(text.contains("alpha"), "{failure}: the data stays: {text}");
        assert!(text.contains("Outdated"), "{failure}: and is marked: {text}");
        assert_eq!(out["frames"][2]["settled"]["buttons"].as_array().unwrap().iter().filter(|b| *b == "Try again").count(), 1, "{failure}: {out}");
    }
}

#[test]
fn an_unknown_and_an_unauthorised_page_look_the_same_and_keep_the_token() {
    let mut pages = Vec::new();
    for status in [404, 403] {
        let Some(out) = run(json!([list(), {"status": status}]), json!([{"submit": TOKEN}, {"go": "/reviews/run-1"}])) else { return };
        let frame = &out["frames"][2];
        assert!(shown(frame).contains("Not found"), "{status}: {out}");
        assert!(frame["settled"]["inputs"].as_array().unwrap().is_empty(), "{status}: {out}");
        pages.push(shown(frame));
    }
    assert_eq!(pages[0], pages[1], "the two must not be told apart");
}
