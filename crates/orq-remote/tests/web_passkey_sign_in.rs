//! The sign-in page and passkeys: what it offers when the server knows passkeys, and what it says
//! when the browser or the connection cannot use them.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run_with(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn page(auth: Value, webauthn: Value, steps: Value) -> Option<Value> {
    run_with(json!({"url": "/", "token": TOKEN, "auth": auth, "webauthn": webauthn, "responses": [], "steps": steps}))
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn buttons(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn over_a_connection_that_is_not_https_the_page_says_passkeys_need_https() {
    let Some(out) = page(json!({"passkeys": true, "signed_in": false}), json!({"secure": false, "api": true}), json!([])) else { return };
    let text = shown(&out["frames"][0]);
    assert!(text.contains("Passkeys need an HTTPS connection"), "{out}");
    assert!(!buttons(&out["frames"][0]).iter().any(|b| b.contains("Sign in with a passkey")), "{out}");
}

#[test]
fn once_a_passkey_exists_and_the_browser_can_use_it_the_page_offers_it_and_no_longer_asks_for_the_token() {
    let Some(out) = page(json!({"passkeys": true, "signed_in": false}), json!({"secure": true, "api": true}), json!([])) else { return };
    assert!(buttons(&out["frames"][0]).iter().any(|b| b.contains("Sign in with a passkey")), "{out}");
    assert!(out["frames"][0]["settled"]["inputs"].as_array().unwrap().is_empty(), "no token field: {out}");
    let text = shown(&out["frames"][0]);
    assert!(!text.contains("Passkeys need") && !text.contains("cannot use passkeys"), "{text}");
}

fn inbox() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "alpha", "status": "approved", "stale": false}]}})
}

#[test]
fn signed_in_with_the_token_where_passkeys_cannot_work_the_page_says_so_instead_of_offering_to_add_one() {
    let scenario = json!({"url": "/", "token": TOKEN, "auth": {"passkeys": false, "signed_in": false}, "webauthn": {"secure": false, "api": true}, "responses": [inbox()], "steps": [{"submit": TOKEN}]});
    let Some(out) = run_with(scenario) else { return };
    assert!(shown(&out["frames"][1]).contains("alpha"), "signed in: {out}");
    assert_eq!(out["frames"][1]["settled"]["passkeyNotes"], json!(["Passkeys need an HTTPS connection to this page."]), "{out}");
    assert_eq!(out["frames"][1]["settled"]["enrolButtons"], json!([]), "{out}");
}

#[test]
fn with_no_passkey_yet_the_token_signs_in_once_and_the_page_offers_to_add_one() {
    let scenario = json!({"url": "/", "token": TOKEN, "auth": {"passkeys": false, "signed_in": false}, "webauthn": {"secure": true, "api": true}, "responses": [inbox()], "steps": [{"submit": TOKEN}]});
    let Some(out) = run_with(scenario) else { return };
    assert_eq!(out["frames"][0]["settled"]["inputs"].as_array().unwrap().len(), 1, "the token is asked for: {out}");
    assert!(shown(&out["frames"][0]).find("HTTPS").is_none(), "{out}");
    assert_eq!(out["frames"][1]["settled"]["enrolButtons"], json!(["Add a passkey for this device"]), "{out}");
    assert_eq!(out["frames"][1]["settled"]["passkeyNotes"], json!([]), "{out}");
}

#[test]
fn signing_in_with_a_passkey_sends_only_the_answer_to_the_servers_challenge_and_keeps_nothing() {
    let scenario = json!({
        "url": "/", "token": TOKEN, "auth": {"passkeys": true, "signed_in": false},
        "webauthn": {"secure": true, "api": true, "answer": {"rawId": [1, 2], "clientDataJSON": [3, 4], "authenticatorData": [5, 6], "signature": [7, 8]}},
        "auth_replies": {
            "/auth/login/begin": {"challenge": "AQID", "rpId": "box.example.ts.net", "userVerification": "required", "timeout": 60000, "allowCredentials": [{"type": "public-key", "id": "AQI"}]},
            "/auth/login/finish": {}
        },
        "responses": [], "steps": [{"click": "button"}]
    });
    let Some(out) = run_with(scenario) else { return };
    let sent: Vec<(String, Value)> = out["authRequests"].as_array().unwrap().iter().map(|r| (r["url"].as_str().unwrap().to_string(), r["body"].clone())).collect();
    assert_eq!(sent[1], ("/auth/login/begin".to_string(), json!({})), "{out}");
    assert_eq!(sent[2].0, "/auth/login/finish", "{out}");
    let mut keys: Vec<&String> = sent[2].1.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["authenticatorData", "clientDataJSON", "id", "signature"], "{out}");
    let after = &out["frames"][1]["settled"];
    assert_eq!(after["sessionKept"], Value::Null, "{out}");
    assert_eq!(after["localKept"], Value::Null, "{out}");
}

fn login(webauthn: Value, failures: Value) -> Option<Value> {
    run_with(json!({
        "url": "/", "token": TOKEN, "auth": {"passkeys": true, "signed_in": false},
        "webauthn": webauthn, "auth_failures": failures,
        "auth_replies": {
            "/auth/login/begin": {"challenge": "AQID", "rpId": "box.example.ts.net", "userVerification": "required", "timeout": 60000, "allowCredentials": [{"type": "public-key", "id": "AQI"}]},
            "/auth/login/finish": {}
        },
        "responses": [], "steps": [{"click": "button"}]
    }))
}

fn answer() -> Value {
    json!({"rawId": [1, 2], "clientDataJSON": [3, 4], "authenticatorData": [5, 6], "signature": [7, 8]})
}

/// A failed sign-in leaves the page signed out with the passkey button still there to try again.
fn assert_refused_and_retryable(out: &Value, finish_sent: bool) {
    let after = &out["frames"][1];
    assert!(shown(after).contains("Could not sign in with a passkey."), "{out}");
    assert!(buttons(after).iter().any(|b| b.contains("Sign in with a passkey")), "retry is possible: {out}");
    assert_eq!(after["settled"]["shell"], false, "still signed out: {out}");
    assert_eq!(after["settled"]["sessionKept"], Value::Null, "{out}");
    let urls: Vec<&str> = out["authRequests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls.contains(&"/auth/login/finish"), finish_sent, "{urls:?}");
}

#[test]
fn a_dismissed_passkey_prompt_signs_nobody_in_sends_no_answer_and_can_be_tried_again() {
    let Some(out) = login(json!({"secure": true, "api": true, "cancel": true}), json!({})) else { return };
    assert_refused_and_retryable(&out, false);
}

#[test]
fn a_prompt_that_gives_no_credential_sends_no_answer() {
    let Some(out) = login(json!({"secure": true, "api": true}), json!({})) else { return };
    assert_refused_and_retryable(&out, false);
}

#[test]
fn a_server_that_refuses_to_begin_a_sign_in_signs_nobody_in_and_can_be_tried_again() {
    let Some(out) = login(json!({"secure": true, "api": true, "answer": answer()}), json!({"/auth/login/begin": 500})) else { return };
    assert_refused_and_retryable(&out, false);
}

#[test]
fn a_server_that_cannot_be_reached_to_begin_a_sign_in_signs_nobody_in_and_can_be_tried_again() {
    let Some(out) = login(json!({"secure": true, "api": true, "answer": answer()}), json!({"/auth/login/begin": "reject"})) else { return };
    assert_refused_and_retryable(&out, false);
}

#[test]
fn a_server_that_refuses_the_answer_signs_nobody_in_and_can_be_tried_again() {
    let Some(out) = login(json!({"secure": true, "api": true, "answer": answer()}), json!({"/auth/login/finish": 401})) else { return };
    assert_refused_and_retryable(&out, true);
}
