//! The Remote actions switch on Settings. Turning off asks first and sends `{on:false, version}`;
//! turning on asks the passkey (a fresh challenge for "remote_actions.on") and sends the answer with
//! `{on:true, version, assertion}`. Signed in with the token, the row says turning on needs a passkey
//! sign-in and offers no way to do it. After a change the settings are read again.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const OFF_QUESTION: &str = "Turn remote actions off? Start, Pause, Resume and the other actions disappear until they are turned on again.";
const SELECTORS: [&str; 4] = ["[data-actions-on]", "[data-actions-off]", "[data-confirm]", "[data-cancel]"];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings(on: bool) -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

fn session() -> Value {
    json!({"passkeys": true, "signed_in": true, "csrf": "c-1"})
}

fn options() -> Value {
    json!({"challenge": "AQID", "rpId": "box.example", "userVerification": "required", "timeout": 120000, "allowCredentials": [{"type": "public-key", "id": "BAUG"}]})
}

fn answer() -> Value {
    json!({"rawId": [1, 2, 3], "clientDataJSON": [4, 5, 6], "authenticatorData": [7, 8, 9], "signature": [10, 11, 12]})
}

/// The Settings page signed in with a passkey session; `webauthn` is what the browser can do.
fn as_session(responses: Value, webauthn: Value, steps: Value) -> Option<Value> {
    run(json!({"url": "/settings", "responses": responses, "auth": session(), "auth_replies": {"/auth/confirm/begin": options()}, "webauthn": webauthn, "steps": steps, "selectors": SELECTORS}))
}

/// The Settings page signed in with the token.
fn as_token(responses: Value, steps: Value) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps.as_array().unwrap().iter().cloned());
    run(json!({"url": "/settings", "responses": responses, "steps": all, "selectors": SELECTORS}))
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
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn body(request: &Value) -> Value {
    serde_json::from_str(request["body"].as_str().unwrap()).unwrap()
}

#[test]
fn signed_in_with_a_passkey_the_off_row_offers_to_turn_on_and_says_it_asks_for_the_passkey() {
    let Some(out) = as_session(json!([settings(false)]), json!({"secure": true, "api": true, "answer": answer()}), json!([])) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Turning on asks for your passkey."), "{out}");
    assert_eq!(count(frame, "[data-actions-on]"), 1, "{out}");
    assert_eq!(count(frame, "[data-actions-off]"), 0, "{out}");
    assert!(posts(&out).is_empty(), "nothing is sent before a click: {out}");
}

#[test]
fn turning_on_asks_first_then_answers_a_fresh_passkey_challenge_and_sends_the_answer_with_the_version() {
    let ok = json!({"status": 200, "json": {"remote_actions": true}});
    let steps = json!([{"click": "[data-actions-on]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!([settings(false), ok, settings(true)]), json!({"secure": true, "api": true, "answer": answer()}), steps) else { return };
    assert_eq!(count(&out["frames"][1], "[data-confirm]"), 1, "asks before the passkey prompt: {out}");
    let begin: Vec<&Value> = out["authRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/auth/confirm/begin").collect();
    assert_eq!(begin.len(), 1, "{out}");
    assert_eq!(begin[0]["method"], "POST");
    assert_eq!(begin[0]["body"], json!({"purpose": "remote_actions.on"}));
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/settings/remote-actions");
    assert_eq!(sent[0]["headers"]["X-CSRF-Token"], "c-1");
    assert!(sent[0]["headers"].get("Authorization").is_none(), "{out}");
    assert_eq!(body(&sent[0]), json!({"on": true, "version": "v-1", "assertion": {"id": "AQID", "clientDataJSON": "BAUG", "authenticatorData": "BwgJ", "signature": "CgsM"}}));
    let reads = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/settings").count();
    assert_eq!(reads, 2, "the settings are read again after the change: {out}");
    let frame = last(&out);
    assert_eq!(count(frame, "[data-actions-off]"), 1, "the row now offers to turn off: {out}");
    assert!(shown(frame).iter().any(|t| t == "On"), "{out}");
}

#[test]
fn cancelling_the_question_or_the_passkey_prompt_sends_nothing_and_offers_the_switch_again() {
    let Some(out) = as_session(json!([settings(false)]), json!({"secure": true, "api": true, "answer": answer()}), json!([{"click": "[data-actions-on]"}, {"click": "[data-cancel]"}])) else { return };
    assert!(posts(&out).is_empty() && !out["authRequests"].as_array().unwrap().iter().any(|r| r["url"] == "/auth/confirm/begin"), "{out}");
    assert_eq!(count(last(&out), "[data-actions-on]"), 1, "{out}");

    let Some(out) = as_session(json!([settings(false)]), json!({"secure": true, "api": true, "cancel": true}), json!([{"click": "[data-actions-on]"}, {"click": "[data-confirm]"}])) else { return };
    assert!(posts(&out).is_empty(), "a dismissed prompt sends nothing: {out}");
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "The passkey check was cancelled."), "{out}");
    assert_eq!(count(frame, "[data-actions-on]"), 1, "{out}");
}

#[test]
fn a_refused_turn_on_shows_the_servers_reason_and_offers_the_switch_again() {
    let refused = json!({"status": 403, "json": {"error": "The passkey check did not pass"}});
    let steps = json!([{"click": "[data-actions-on]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!([settings(false), refused]), json!({"secure": true, "api": true, "answer": answer()}), steps) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "The passkey check did not pass"), "{out}");
    assert_eq!(count(frame, "[data-actions-on]"), 1, "{out}");
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/settings").count(), 1, "nothing changed, nothing re-read: {out}");
}

#[test]
fn signed_in_with_the_token_the_off_row_says_turning_on_needs_a_passkey_sign_in_and_offers_no_switch() {
    let Some(out) = as_token(json!([settings(false)]), json!([])) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Turning remote actions on needs a passkey sign-in."), "{out}");
    assert_eq!(count(frame, "[data-actions-on]"), 0, "{out}");
    assert!(!shown(frame).iter().any(|t| t == "Turning on asks for your passkey."), "{out}");
}

#[test]
fn turning_off_asks_the_question_then_sends_off_with_the_version_and_reads_the_settings_again() {
    let ok = json!({"status": 200, "json": {"remote_actions": false}});
    let Some(out) = as_token(json!([settings(true), ok, settings(false)]), json!([{"click": "[data-actions-off]"}, {"click": "[data-confirm]"}])) else { return };
    assert!(shown(&out["frames"][2]).iter().any(|t| t == OFF_QUESTION), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/settings/remote-actions");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(body(&sent[0]), json!({"on": false, "version": "v-1"}));
    assert!(out["authRequests"].as_array().unwrap().iter().all(|r| r["url"] != "/auth/confirm/begin"), "turning off needs no passkey: {out}");
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Off"), "{out}");
    assert_eq!(count(frame, "[data-actions-off]"), 0, "{out}");
}

#[test]
fn cancelling_turn_off_sends_nothing() {
    let Some(out) = as_token(json!([settings(true)]), json!([{"click": "[data-actions-off]"}, {"click": "[data-cancel]"}])) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-actions-off]"), 1, "{out}");
}

#[test]
fn settings_changed_elsewhere_says_so_and_offers_reload_which_reads_them_again() {
    let conflict = json!({"status": 409, "json": {"error": "Settings changed since you opened them"}});
    let steps = json!([{"click": "[data-actions-off]"}, {"click": "[data-confirm]"}, {"click": "[data-settings-reload]"}]);
    let Some(out) = as_token(json!([settings(true), conflict, settings(true)]), steps) else { return };
    assert!(shown(&out["frames"][3]).iter().any(|t| t == "Settings changed since you opened them"), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/settings").count(), 2, "Reload reads the settings again: {out}");
    assert!(!shown(last(&out)).iter().any(|t| t == "Settings changed since you opened them"), "{out}");
}

#[test]
fn settings_without_a_version_offer_no_switch() {
    let bare = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true}}});
    let Some(out) = as_token(json!([bare]), json!([])) else { return };
    assert_eq!(count(last(&out), "[data-actions-off]"), 0, "{out}");
    assert!(shown(last(&out)).iter().any(|t| t == "On"), "{out}");
}
