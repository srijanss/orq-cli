//! The sign-in page when the server cannot be reached: it says so and offers Retry, instead of a
//! token field that cannot work without the server.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const UNREACHABLE: &str = "Can't reach orq. Check that this device is connected to Tailscale, then try again.";

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut scenario = scenario;
    scenario["token"] = json!(TOKEN);
    if scenario.get("responses").is_none() {
        scenario["responses"] = json!([]);
    }
    if scenario.get("steps").is_none() {
        scenario["steps"] = json!([]);
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn buttons(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(|t| t.trim().to_string()).collect()
}

fn inputs(frame: &Value) -> usize {
    frame["settled"]["inputs"].as_array().unwrap().len()
}

fn passkey_page() -> Value {
    json!({"secure": true, "api": true})
}

#[test]
fn a_status_request_that_rejects_shows_the_tailscale_text_and_retry_and_nothing_to_sign_in_with() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_failures": {"/auth/status": "reject"}})) else { return };
    let start = &out["frames"][0];
    assert!(shown(start).contains(UNREACHABLE), "{}", shown(start));
    assert!(buttons(start).iter().any(|b| b == "Retry"), "{:?}", buttons(start));
    assert_eq!(inputs(start), 0, "no Access token field: {out}");
    assert!(!buttons(start).iter().any(|b| b.contains("passkey")), "{:?}", buttons(start));
}

#[test]
fn a_bad_gateway_unavailable_or_gateway_timeout_counts_as_unreachable_but_a_404_still_offers_the_token() {
    for status in [502, 503, 504] {
        let Some(out) = run(json!({"url": "/dashboard", "auth_failures": {"/auth/status": status}})) else { return };
        assert!(shown(&out["frames"][0]).contains(UNREACHABLE), "{status}: {}", shown(&out["frames"][0]));
        assert_eq!(inputs(&out["frames"][0]), 0, "{status}: {out}");
    }
    let Some(out) = run(json!({"url": "/dashboard", "auth_failures": {"/auth/status": 404}})) else { return };
    assert!(!shown(&out["frames"][0]).contains(UNREACHABLE), "{}", shown(&out["frames"][0]));
    assert_eq!(inputs(&out["frames"][0]), 1, "the token field: {out}");
}

#[test]
fn retry_once_the_server_answers_offers_the_passkey() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["reject", {"passkeys": true, "signed_in": false}], "steps": [{"click": "button"}]})) else { return };
    let after = &out["frames"][1];
    assert!(!shown(after).contains(UNREACHABLE), "{}", shown(after));
    assert!(buttons(after).iter().any(|b| b == "Sign in with a passkey"), "{:?}", buttons(after));
}

#[test]
fn coming_back_online_tries_again_by_itself() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["reject", {"passkeys": true, "signed_in": false}], "steps": [{"online": true}]})) else { return };
    let after = &out["frames"][1];
    assert!(buttons(after).iter().any(|b| b == "Sign in with a passkey"), "{:?}", buttons(after));
}

#[test]
fn the_page_tries_again_when_shown_again_and_every_fifteen_seconds_while_visible() {
    let answered = json!({"passkeys": true, "signed_in": false});
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["reject", "reject", answered], "steps": [{"visibility": "hidden"}, {"visibility": "visible"}, {"tick": 15000}]})) else { return };
    assert_eq!(out["frames"][0]["settled"]["activeIntervals"], json!([15000]), "{out}");
    assert!(shown(&out["frames"][1]).contains(UNREACHABLE), "hiding asks nothing");
    assert!(shown(&out["frames"][2]).contains(UNREACHABLE), "shown again, asked and still unreachable");
    let after = &out["frames"][3];
    assert!(buttons(after).iter().any(|b| b == "Sign in with a passkey"), "{:?}", buttons(after));
    assert_eq!(after["settled"]["activeIntervals"], json!([]), "no retrying once reachable: {out}");
    assert_eq!(after["settled"]["visibilityListeners"], 0, "{out}");
    assert_eq!(out["authRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/auth/status").count(), 3, "{out}");
}

#[test]
fn a_kept_token_whose_request_rejects_stays_on_the_page_error_and_is_not_sent_to_sign_in() {
    let Some(out) = run(json!({"url": "/dashboard", "session": {"orq.token": TOKEN}, "auth_failures": {"/auth/status": "reject"}, "responses": [{"reject": true}]})) else { return };
    let start = &out["frames"][0];
    assert!(!shown(start).contains(UNREACHABLE), "{}", shown(start));
    assert_eq!(inputs(start), 0, "no token field: {out}");
    assert_eq!(start["settled"]["shell"], true, "still inside the signed-in page: {out}");
    assert_eq!(start["settled"]["sessionKept"], json!({"orq.token": "<the token>"}), "{out}");
}

#[test]
fn the_token_form_button_says_sign_in() {
    let Some(out) = run(json!({"url": "/dashboard"})) else { return };
    let start = &out["frames"][0];
    assert_eq!(inputs(start), 1, "{out}");
    assert!(buttons(start).iter().any(|b| b == "Sign in"), "{:?}", buttons(start));
    assert!(!buttons(start).iter().any(|b| b.contains("Open the inbox")), "{:?}", buttons(start));
}

#[test]
fn a_status_reply_that_is_not_json_counts_as_no_passkey_support_and_offers_the_token() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["badjson"]})) else { return };
    let start = &out["frames"][0];
    assert!(!shown(start).contains(UNREACHABLE), "{}", shown(start));
    assert_eq!(inputs(start), 1, "the token field: {out}");
    assert_eq!(start["settled"]["activeIntervals"], json!([]), "no retrying: {out}");
}

#[test]
fn a_status_request_that_never_answers_gives_up_after_eight_seconds_as_unreachable() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["hold"], "steps": [{"expire": 8000}]})) else { return };
    assert_eq!(out["frames"][0]["settled"]["deadlines"], 1, "the request is under way with its deadline: {out}");
    assert!(!shown(&out["frames"][0]).contains(UNREACHABLE), "not before the deadline");
    let after = &out["frames"][1];
    assert!(shown(after).contains(UNREACHABLE), "{}", shown(after));
    assert_eq!(after["settled"]["deadlines"], 0, "{out}");
}

#[test]
fn a_status_body_that_stalls_gives_up_after_eight_seconds_as_unreachable() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": ["hold_body"], "steps": [{"expire": 8000}]})) else { return };
    assert!(!shown(&out["frames"][0]).contains(UNREACHABLE), "not before the deadline");
    assert!(shown(&out["frames"][1]).contains(UNREACHABLE), "{}", shown(&out["frames"][1]));
}

#[test]
fn a_status_answered_in_time_leaves_no_deadline_behind() {
    let Some(out) = run(json!({"url": "/dashboard", "webauthn": passkey_page(), "auth_sequence": [{"passkeys": true, "signed_in": false}]})) else { return };
    let start = &out["frames"][0];
    assert!(buttons(start).iter().any(|b| b == "Sign in with a passkey"), "{:?}", buttons(start));
    assert_eq!(start["settled"]["deadlines"], 0, "{out}");
}
