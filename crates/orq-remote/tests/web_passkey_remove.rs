//! Signed in with a passkey, each row on the Settings passkey list gets Remove. It asks "Remove {label}?
//! Devices signed in with it are signed out." first; yes sends POST /auth/passkeys/{id}/remove with the
//! session's CSRF token and reads the list again. The row this session signed in with says "This device",
//! and removing it signs the page out. A refusal shows the server's reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const SELECTORS: [&str; 5] = ["[data-passkey-remove]", "[data-confirm]", "[data-cancel]", "[data-sign-out]", "[data-passkey-rename]"];

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

fn passkeys() -> Value {
    json!({"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}, {"label": "laptop", "id": "BBBBlaptopID", "this_device": false}]})
}

fn as_session(replies: Value, failures: Value, steps: Value) -> Option<Value> {
    let mut auth_replies = json!({"/auth/passkeys": passkeys()});
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

const LAPTOP: &str = "/auth/passkeys/BBBBlaptopID/remove";
const PHONE: &str = "/auth/passkeys/AAAAphoneID/remove";

#[test]
fn remove_asks_first_then_sends_with_the_csrf_token_and_reads_the_list_again() {
    let steps = json!([{"click": "[data-passkey-remove=\"BBBBlaptopID\"]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({LAPTOP: {"id": "BBBBlaptopID", "label": "laptop", "signed_out": false}}), json!({}), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], "[data-passkey-remove]"), 2, "each passkey offers Remove: {out}");
    assert!(shown(&frames[1]).iter().any(|t| t == "Remove laptop? Devices signed in with it are signed out."), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], LAPTOP);
    assert_eq!(sent[0]["headers"]["X-CSRF-Token"], "c-1");
    let lists = out["authRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/auth/passkeys").count();
    assert_eq!(lists, 2, "the list is read again after a removal: {out}");
    assert_eq!(count(last(&out), "[data-confirm]"), 0, "{out}");
}

#[test]
fn the_row_this_session_signed_in_with_says_this_device() {
    let Some(out) = as_session(json!({}), json!({}), json!([])) else { return };
    let texts = shown(last(&out));
    assert_eq!(texts.iter().filter(|t| t.as_str() == "This device").count(), 1, "{out}");
}

#[test]
fn cancel_sends_nothing() {
    let steps = json!([{"click": "[data-passkey-remove=\"BBBBlaptopID\"]"}, {"click": "[data-cancel]"}]);
    let Some(out) = as_session(json!({}), json!({}), steps) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-passkey-remove]"), 2, "{out}");
}

#[test]
fn removing_this_devices_passkey_signs_the_page_out() {
    let steps = json!([{"click": "[data-passkey-remove=\"AAAAphoneID\"]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({PHONE: {"id": "AAAAphoneID", "label": "phone", "signed_out": true}}), json!({}), steps) else { return };
    let urls: Vec<String> = posts(&out).iter().map(|r| r["url"].as_str().unwrap().to_string()).collect();
    assert_eq!(urls.first().map(String::as_str), Some(PHONE), "{out}");
    assert!(urls.iter().any(|url| url == "/auth/logout"), "the page signs out: {out}");
}

#[test]
fn a_refusal_shows_the_servers_reason() {
    let last_one = json!({LAPTOP: {"status": 409, "json": {"error": "Add another passkey before removing this one"}}});
    let steps = json!([{"click": "[data-passkey-remove=\"BBBBlaptopID\"]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({}), last_one, steps) else { return };
    assert!(shown(last(&out)).iter().any(|t| t == "Add another passkey before removing this one"), "{out}");
}

#[test]
fn a_removal_that_never_reaches_the_server_says_so_and_keeps_the_list() {
    let steps = json!([{"click": "[data-passkey-remove=\"BBBBlaptopID\"]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!({}), json!({LAPTOP: "reject"}), steps) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "The passkey could not be removed."), "{out}");
    assert_eq!(count(frame, "[data-passkey-remove]"), 2, "{out}");
    assert!(!posts(&out).iter().any(|r| r["url"] == "/auth/logout"), "{out}");
}
