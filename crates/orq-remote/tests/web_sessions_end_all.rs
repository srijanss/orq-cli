//! Signed in with a passkey, Settings has "Sign out every device". It asks "Sign out every device? You will
//! need your passkey or the token to sign in again." first; yes sends POST /auth/sessions/end-all with the
//! session's CSRF token and the page signs out. A refusal shows the server's reason. Signed in with the
//! token alone (no passkey session), it is not offered.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const SELECTORS: [&str; 4] = ["[data-sign-out-everywhere]", "[data-confirm]", "[data-cancel]", "[data-sign-out]"];
const PATH: &str = "/auth/sessions/end-all";
const QUESTION: &str = "Sign out every device? You will need your passkey or the token to sign in again.";

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings() -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": false, "version": "v-1"}}})
}

fn as_session(replies: Value, failures: Value, steps: Value) -> Option<Value> {
    let mut auth_replies = json!({"/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]}});
    for (path, reply) in replies.as_object().unwrap() {
        auth_replies[path] = reply.clone();
    }
    run(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": auth_replies, "auth_failures": failures, "responses": [settings()], "steps": steps, "selectors": SELECTORS
    }))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["authRequests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn sign_out_everywhere_asks_first_then_ends_every_session_and_signs_the_page_out() {
    let steps = json!([{"click": "[data-sign-out-everywhere]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({PATH: {"signed_out": true, "ended": 2}}), json!({}), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], "[data-sign-out-everywhere]"), 1, "{out}");
    assert!(shown(&frames[1]).iter().any(|t| t == QUESTION), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.first().map(|r| r["url"].clone()), Some(json!(PATH)), "{out}");
    assert_eq!(sent[0]["headers"]["X-CSRF-Token"], "c-1");
    assert!(sent.iter().any(|r| r["url"] == "/auth/logout"), "the page signs out: {out}");
}

#[test]
fn cancel_sends_nothing() {
    let steps = json!([{"click": "[data-sign-out-everywhere]"}, {"click": "[data-cancel]"}]);
    let Some(out) = as_session(json!({}), json!({}), steps) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-sign-out-everywhere]"), 1, "{out}");
}

#[test]
fn a_refusal_shows_the_servers_reason_and_stays_signed_in() {
    let refused = json!({PATH: {"status": 403, "json": {"error": "not allowed"}}});
    let steps = json!([{"click": "[data-sign-out-everywhere]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({}), refused, steps) else { return };
    assert!(shown(last(&out)).iter().any(|t| t == "not allowed"), "{out}");
    assert!(!posts(&out).iter().any(|r| r["url"] == "/auth/logout"), "{out}");
}

#[test]
fn signed_in_with_the_token_alone_it_is_not_offered() {
    let Some(out) = run(json!({
        "url": "/settings", "token": "tok", "session": {"orq.token": "tok"}, "auth": {"passkeys": true, "signed_in": false},
        "responses": [settings()], "steps": [], "selectors": SELECTORS
    })) else { return };
    assert_eq!(count(last(&out), "[data-sign-out-everywhere]"), 0, "{out}");
    assert!(count(last(&out), "[data-sign-out]") >= 1, "Sign out is still there: {out}");
}
