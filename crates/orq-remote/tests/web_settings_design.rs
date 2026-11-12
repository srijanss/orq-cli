//! Settings reads as label/value rows. An agent is edited in place: Edit (with its pencil) turns the Harness
//! and Model values of that card into selects in their own rows. Every control sits in a row beside its label,
//! not alone on a line. Every question is a confirm block: a primary answer for a change, a danger answer for
//! what removes something or signs devices out. An empty cleanup preview says there is nothing to prune.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = [
    "button[data-agent-edit] svg",
    ".settings-row dd select[data-agent-harness]",
    ".settings-row dd select[data-agent-model]",
    ".confirm .confirm-question + .confirm-actions button.primary-button[data-confirm]",
    ".confirm .confirm-question + .confirm-actions button.danger-button[data-confirm]",
    "[data-remote-actions] dd button[data-actions-off]",
    ".settings-row > .settings-label + button[data-storage-preview]",
    "[data-storage-prune]",
    "[data-agent-harness]",
];

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
    let implementer = json!({"harness": "claude", "model": "sonnet", "effort": null});
    json!({"status": 200, "json": {"settings": {"implementer": implementer, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

fn options() -> Value {
    json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus", "sonnet"]}, {"harness": "pi", "models": ["gpt-5"]}]}})
}

fn preview(files: u64, lines: Vec<&str>) -> Value {
    json!({"status": 200, "json": {"lines": lines, "files": files, "bytes": 2560, "size": "2.5 KiB", "runs": 2, "metadata": 0, "hash": "h1"}})
}

fn as_token(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/settings", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn an_agent_is_edited_in_place_in_its_own_rows_and_save_asks_in_a_primary_confirm() {
    let steps = vec![json!({"click": "[data-agent-edit=\"implementer\"]"}), json!({"click": "[data-agent-save]"})];
    let Some(out) = as_token(vec![settings(true), options()], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 2, "both Edit buttons carry the pencil: {out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "the harness is chosen in its own row: {out}");
    assert_eq!(count(&frames[2], SELECTORS[2]), 1, "the model is chosen in its own row: {out}");
    assert_eq!(count(&frames[2], SELECTORS[8]), 1, "{out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 1, "{out}");
}

#[test]
fn every_control_sits_beside_its_label_and_turning_actions_off_asks_in_a_confirm() {
    let Some(out) = as_token(vec![settings(true)], vec![json!({"click": "[data-actions-off]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[5]), 1, "Turn off sits in the Remote actions row: {out}");
    assert_eq!(count(&frames[1], SELECTORS[6]), 1, "Preview cleanup sits beside its label: {out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 1, "{out}");
}

#[test]
fn an_empty_cleanup_says_there_is_nothing_to_prune_once() {
    for lines in [vec![], vec!["Nothing to prune."]] {
        let Some(out) = as_token(vec![settings(true), preview(0, lines.clone())], vec![json!({"click": "[data-storage-preview]"})]) else { return };
        let said = shown(last(&out)).iter().filter(|t| t.as_str() == "Nothing to prune.").count();
        assert_eq!(said, 1, "{lines:?}: {out}");
        assert_eq!(count(last(&out), SELECTORS[7]), 0, "{out}");
    }
}

#[test]
fn cleaning_up_asks_in_a_danger_confirm() {
    let steps = vec![json!({"click": "[data-storage-preview]"}), json!({"click": "[data-storage-prune]"})];
    let Some(out) = as_token(vec![settings(true), preview(3, vec!["3 file(s)"])], steps) else { return };
    assert_eq!(count(last(&out), SELECTORS[4]), 1, "{out}");
}

#[test]
fn signing_out_everywhere_and_removing_a_passkey_ask_in_a_danger_confirm() {
    let session = |click: &str| {
        run(json!({
            "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
            "auth_replies": {"/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]}},
            "auth_failures": {}, "responses": [settings(false)], "steps": [{"click": click}], "selectors": SELECTORS
        }))
    };
    for click in ["[data-sign-out-everywhere]", "[data-passkey-remove]"] {
        let Some(out) = session(click) else { return };
        assert_eq!(count(last(&out), SELECTORS[4]), 1, "{click}: {out}");
    }
}
