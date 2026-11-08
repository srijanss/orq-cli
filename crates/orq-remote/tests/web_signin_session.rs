//! The sign-in page's identity, and how long a sign-in lasts.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    run_with(json!({"url": url, "token": TOKEN, "responses": responses, "steps": steps}))
}

fn run_with(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn the_sign_in_page_shows_the_logo_and_the_product_name() {
    let Some(out) = run("/", json!([]), json!([])) else { return };
    let start = &out["frames"][0]["settled"];
    assert_eq!(start["brandLogos"], 1, "{out}");
    let texts: Vec<&str> = start["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Orq"), "{texts:?}");
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "alpha", "status": "approved", "stale": false}]}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn signing_in_keeps_the_token_for_this_tab_only() {
    let Some(out) = run("/", json!([list()]), json!([{"submit": TOKEN}])) else { return };
    let after = &out["frames"][1]["settled"];
    assert_eq!(after["sessionKept"], json!({"orq.token": "<the token>"}), "{out}");
    assert_eq!(after["localKept"], Value::Null, "nothing outlives the tab: {out}");
    assert!(!out["frames"][1]["settled"]["path"].as_str().unwrap().contains(TOKEN));
}

#[test]
fn a_refresh_with_a_kept_token_goes_straight_to_the_page_without_asking() {
    let Some(out) = run_with(json!({"url": "/", "token": TOKEN, "session": {"orq.token": TOKEN}, "responses": [list()], "steps": []})) else { return };
    let start = &out["frames"][0]["settled"];
    assert!(start["inputs"].as_array().unwrap().is_empty(), "{out}");
    assert!(shown(&out["frames"][0]).contains("alpha"), "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
}

#[test]
fn a_refused_kept_token_is_forgotten_and_asked_for_again() {
    let Some(out) = run_with(json!({"url": "/", "token": TOKEN, "session": {"orq.token": TOKEN}, "responses": [{"status": 401}], "steps": []})) else { return };
    let start = &out["frames"][0]["settled"];
    assert_eq!(start["inputs"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(start["sessionKept"], Value::Null, "the refused token is gone: {out}");
    assert!(shown(&out["frames"][0]).contains("Could not sign in"), "{out}");
}

#[test]
fn an_unreachable_server_does_not_forget_the_kept_token() {
    let Some(out) = run_with(json!({"url": "/", "token": TOKEN, "session": {"orq.token": TOKEN}, "responses": [{"reject": true}], "steps": []})) else { return };
    assert_eq!(out["frames"][0]["settled"]["sessionKept"], json!({"orq.token": "<the token>"}), "{out}");
}

#[test]
fn storage_that_cannot_be_used_still_lets_you_sign_in_for_this_page_view() {
    let Some(out) = run_with(json!({"url": "/", "storage_blocked": true, "responses": [list()], "steps": [{"submit": TOKEN}]})) else { return };
    assert!(shown(&out["frames"][1]).contains("alpha"), "{out}");
}

#[test]
fn signing_out_forgets_the_token_and_asks_for_it_again_without_an_error_message() {
    let Some(out) = run("/dashboard", json!([{"status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}, {"nav": "[data-sign-out]"}])) else { return };
    let before = &out["frames"][1]["settled"];
    assert_eq!(before["sessionKept"], json!({"orq.token": "<the token>"}), "{out}");
    let after = &out["frames"][2]["settled"];
    assert_eq!(after["inputs"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(after["sessionKept"], Value::Null, "{out}");
    assert!(!shown(&out["frames"][2]).contains("Could not sign in"), "{out}");
    assert_eq!(after["shell"], false, "the shell goes with the session: {out}");
}

#[test]
fn after_signing_out_a_refresh_asks_again_and_a_new_sign_in_works() {
    let Some(out) = run("/dashboard", json!([{"status": 200, "json": {"projects": []}}, {"status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}, {"nav": "[data-sign-out]"}, {"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(out["frames"][3]["settled"]["sessionKept"], json!({"orq.token": "<the token>"}), "{out}");
}
