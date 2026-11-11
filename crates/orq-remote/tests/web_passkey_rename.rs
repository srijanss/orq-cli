//! Signed in with a passkey, each row on the Settings passkey list gets Rename: it opens a name field
//! with the current name, Save sends POST /auth/passkeys/{id}/label `{label}` with the session's CSRF
//! token, and the list is read again. A refusal shows the server's reason and keeps the field open;
//! Cancel closes it and sends nothing.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const SELECTORS: [&str; 4] = ["[data-passkey-rename]", "[data-passkey-label]", "[data-passkey-save]", "[data-cancel]"];

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
    json!({"passkeys": [{"label": "phone", "id": "AAAAphoneID"}, {"label": "laptop", "id": "BBBBlaptopID"}]})
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

fn renames(out: &Value) -> Vec<Value> {
    out["authRequests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

const PATH: &str = "/auth/passkeys/BBBBlaptopID/label";

#[test]
fn rename_opens_the_name_field_and_save_sends_the_label_with_the_csrf_token_then_reads_the_list_again() {
    let steps = json!([
        {"click": "[data-passkey-rename=\"BBBBlaptopID\"]"},
        {"set": "[data-passkey-label]", "value": "Work laptop"},
        {"click": "[data-passkey-save]"},
    ]);
    let Some(out) = as_session(json!({PATH: {"id": "BBBBlaptopID", "label": "Work laptop"}}), json!({}), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], "[data-passkey-rename]"), 2, "each passkey offers Rename: {out}");
    assert_eq!(count(&frames[1], "[data-passkey-label]"), 1, "{out}");
    let sent = renames(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], PATH);
    assert_eq!(sent[0]["body"], json!({"label": "Work laptop"}));
    assert_eq!(sent[0]["headers"]["X-CSRF-Token"], "c-1");
    let lists = out["authRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/auth/passkeys").count();
    assert_eq!(lists, 2, "the list is read again after a rename: {out}");
    assert_eq!(count(last(&out), "[data-passkey-label]"), 0, "the field closes: {out}");
}

#[test]
fn the_name_field_starts_with_the_current_name() {
    let Some(out) = as_session(json!({}), json!({}), json!([{"click": "[data-passkey-rename=\"AAAAphoneID\"]"}])) else { return };
    let inputs = last(&out)["settled"]["inputs"].as_array().unwrap().clone();
    assert!(inputs.iter().any(|input| input["value"] == "phone"), "{out}");
}

#[test]
fn a_refusal_shows_the_servers_reason_and_keeps_the_field_open() {
    let taken = json!({PATH: {"status": 409, "json": {"error": "Another passkey already has that name"}}});
    let steps = json!([{"click": "[data-passkey-rename=\"BBBBlaptopID\"]"}, {"set": "[data-passkey-label]", "value": "phone"}, {"click": "[data-passkey-save]"}]);
    let Some(out) = as_session(json!({}), taken, steps) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Another passkey already has that name"), "{out}");
    assert_eq!(count(frame, "[data-passkey-label]"), 1, "{out}");
}

#[test]
fn cancel_closes_the_field_and_sends_nothing() {
    let steps = json!([{"click": "[data-passkey-rename=\"BBBBlaptopID\"]"}, {"click": "[data-cancel]"}]);
    let Some(out) = as_session(json!({}), json!({}), steps) else { return };
    assert!(renames(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-passkey-label]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-passkey-rename]"), 2, "{out}");
}

#[test]
fn a_rename_that_never_reaches_the_server_says_so_and_keeps_the_field_open() {
    let steps = json!([{"click": "[data-passkey-rename=\"BBBBlaptopID\"]"}, {"set": "[data-passkey-label]", "value": "Work"}, {"click": "[data-passkey-save]"}]);
    let Some(out) = as_session(json!({}), json!({PATH: "reject"}), steps) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "The passkey could not be renamed."), "{out}");
    assert_eq!(count(frame, "[data-passkey-label]"), 1, "{out}");
}
