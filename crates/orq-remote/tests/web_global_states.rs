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
    assert!(!first["settled"]["texts"].to_string().contains(TOKEN) && !first["settled"]["inputs"].to_string().contains(TOKEN), "the page never shows the token");
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

#[test]
fn the_not_found_screen_offers_a_way_back_to_the_inbox() {
    for status in [404, 403] {
        let Some(out) = run(json!([list(), {"status": status}, list()]), json!([{"submit": TOKEN}, {"go": "/reviews/run-1"}, {"click": "button", "index": 0}])) else { return };
        assert_eq!(out["frames"][2]["settled"]["buttons"], json!(["Back to the list"]), "{status}: {out}");
        let back = &out["frames"][3]["settled"];
        assert_eq!(back["path"], "/", "{status}: {out}");
        assert!(shown(&out["frames"][3]).contains("alpha"), "{status}: {out}");
    }
}

#[test]
fn retrying_after_a_failed_refresh_replaces_the_old_data_and_clears_the_markers() {
    let fresh = json!({"status": 200, "json": {"reviews": [{"run_id": "run-2", "feature": "beta", "status": "approved", "stale": false}]}});
    for failure in [json!({"status": 503}), json!({"reject": true}), json!({"status": 200, "json": {"nope": 1}})] {
        let Some(out) = run(json!([list(), failure.clone(), fresh.clone()]), json!([{"submit": TOKEN}, {"nav": "[data-refresh]"}, {"click": "button", "index": 0}])) else { return };
        assert!(shown(&out["frames"][2]).contains("Outdated"), "{failure}: {out}");
        let text = shown(&out["frames"][3]);
        assert!(text.contains("beta") && !text.contains("alpha"), "{failure}: {text}");
        assert!(!text.contains("Outdated") && !text.contains("Disconnected") && !text.contains("could not be read"), "{failure}: {text}");
        assert_eq!(out["requests"][2]["headers"], out["requests"][0]["headers"], "{failure}");
    }
}

fn list_of(feature: &str) -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-2", "feature": feature, "status": "approved", "stale": false}]}})
}

#[test]
fn a_successful_retry_after_a_failed_refresh_replaces_the_old_data_and_clears_the_warnings() {
    // A 503, a dropped connection and an unreadable body all leave the old data marked outdated.
    for failure in [json!({"status": 503}), json!({"reject": true}), json!({"status": 200, "badjson": true})] {
        let responses = json!([list(), failure.clone(), list_of("newer-feature")]);
        let Some(first) = run(responses.clone(), json!([{"submit": TOKEN}, {"nav": "[data-refresh]"}])) else { return };
        let buttons = first["frames"][2]["settled"]["buttons"].as_array().unwrap().clone();
        let retry = buttons.iter().position(|b| b == "Try again").unwrap_or_else(|| panic!("{failure}: no retry: {first}"));
        assert!(shown(&first["frames"][2]).contains("Outdated") && shown(&first["frames"][2]).contains("alpha"), "{failure}: {first}");
        let Some(out) = run(responses, json!([{"submit": TOKEN}, {"nav": "[data-refresh]"}, {"click": "button", "index": retry}])) else { return };
        let text = shown(&out["frames"][3]);
        assert!(text.contains("newer-feature") && !text.contains("alpha"), "{failure}: the new data replaces the old: {text}");
        assert!(!text.contains("Outdated") && !text.contains("Disconnected"), "{failure}: the warnings are gone: {text}");
        assert!(!out["frames"][3]["settled"]["buttons"].as_array().unwrap().iter().any(|b| b == "Try again"), "{failure}: {out}");
        assert_eq!(out["requests"][2]["headers"]["Authorization"], format!("Bearer {TOKEN}"), "{failure}: the retry reuses the held token");
    }
}
