//! The Implementer and Reviewer cards on Settings get Edit while remote actions are on: it reads the
//! options, offers a harness select and a model select from them, then Save asks
//! "Save implementer: {harness} {model}?" and sends `{implementer: {harness, model}, version}`. A refusal
//! shows the server's reason; a 409 offers Reload. After a save the settings are read again.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = ["[data-agent-edit]", "[data-agent-harness]", "[data-agent-model]", "[data-agent-save]", "[data-confirm]", "[data-cancel]", "[data-settings-reload]"];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings(on: bool, implementer_model: &str) -> Value {
    let implementer = json!({"harness": "claude", "model": implementer_model, "effort": null});
    json!({"status": 200, "json": {"settings": {"implementer": implementer, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

fn options() -> Value {
    json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus", "sonnet"]}, {"harness": "pi", "models": ["gpt-5", "gpt-5-mini"]}]}})
}

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
fn edit_offers_the_options_and_save_asks_then_sends_the_choice_with_the_version() {
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = json!([
        {"click": "[data-agent-edit=\"implementer\"]"},
        {"set": "[data-agent-harness]", "value": "pi"},
        {"click": "[data-agent-save]"},
        {"click": "[data-confirm]"},
    ]);
    let Some(out) = as_token(json!([settings(true, "sonnet"), options(), ok, settings(true, "gpt-5")]), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], "[data-agent-edit]"), 2, "both cards offer Edit: {out}");
    assert_eq!(count(&frames[2], "[data-agent-harness]"), 1, "{out}");
    assert_eq!(count(&frames[2], "[data-agent-model]"), 1, "{out}");
    let reads: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/settings/options").collect();
    assert_eq!(reads.len(), 1, "Edit reads the options: {out}");
    assert_eq!(reads[0]["method"], "GET");
    assert!(shown(&frames[4]).iter().any(|t| t == "Save implementer: pi gpt-5?"), "a new harness starts on its first model, and Save asks first: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/settings/agents");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(body(&sent[0]), json!({"implementer": {"harness": "pi", "model": "gpt-5"}, "version": "v-1"}));
    let settings_reads = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/settings").count();
    assert_eq!(settings_reads, 2, "the settings are read again after a save: {out}");
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "gpt-5"), "{out}");
    assert_eq!(count(frame, "[data-agent-harness]"), 0, "the editor closes: {out}");
}

#[test]
fn with_remote_actions_off_there_is_no_edit() {
    let Some(out) = as_token(json!([settings(false, "sonnet")]), json!([])) else { return };
    assert_eq!(count(last(&out), "[data-agent-edit]"), 0, "{out}");
}

#[test]
fn cancel_closes_the_editor_and_sends_nothing() {
    let steps = json!([{"click": "[data-agent-edit=\"implementer\"]"}, {"click": "[data-cancel]"}]);
    let Some(out) = as_token(json!([settings(true, "sonnet"), options()]), steps) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-agent-harness]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-agent-edit]"), 2, "{out}");
}

#[test]
fn a_stale_version_shows_the_reason_and_offers_reload() {
    let stale = json!({"status": 409, "json": {"error": "The settings changed since they were read."}});
    let steps = json!([
        {"click": "[data-agent-edit=\"implementer\"]"},
        {"set": "[data-agent-model]", "value": "opus"},
        {"click": "[data-agent-save]"},
        {"click": "[data-confirm]"},
    ]);
    let Some(out) = as_token(json!([settings(true, "sonnet"), options(), stale]), steps) else { return };
    assert_eq!(body(&posts(&out)[0]), json!({"implementer": {"harness": "claude", "model": "opus"}, "version": "v-1"}));
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "The settings changed since they were read."), "{out}");
    assert_eq!(count(frame, "[data-settings-reload]"), 1, "{out}");
}
