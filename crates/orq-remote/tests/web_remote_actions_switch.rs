//! Remote actions is one switch in its row, like the logs' Follow switch: the track and the word On or Off,
//! with no pill beside a separate Turn off or Turn on button. Flipping it still asks first (and turning on
//! still asks the passkey). It is greyed out while its question is open, and when the page cannot turn
//! remote actions on: signed in with the token, or without the settings' version.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = [
    "[data-remote-actions] dd button[role=switch][aria-checked=true][data-actions-off] .switch-track",
    "[data-remote-actions] dd button[role=switch][aria-checked=false][data-actions-on] .switch-track",
    "[data-remote-actions] dd button[role=switch][aria-checked=false]:disabled",
    "[data-remote-actions] button[role=switch]:disabled",
    "[data-remote-actions] .pill",
    "[data-remote-actions] button:not([role=switch])",
    ".confirm [data-confirm]",
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

fn settings(on: bool, version: bool) -> Value {
    let mut body = json!({"implementer": null, "reviewer": null, "models": [], "remote_actions": on});
    if version {
        body["version"] = json!("v-1");
    }
    json!({"status": 200, "json": {"settings": body}})
}

fn as_token(on: bool, version: bool, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/settings", "responses": [settings(on, version)], "steps": all, "selectors": SELECTORS}))
}

fn as_session(steps: Vec<Value>) -> Option<Value> {
    let auth = json!({"passkeys": true, "signed_in": true, "csrf": "c-1"});
    run(json!({"url": "/settings", "responses": [settings(false, true)], "auth": auth, "webauthn": {"secure": true, "api": true}, "steps": steps, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn on_is_one_switch_with_its_word_and_no_pill_or_extra_button_and_flipping_it_asks_first() {
    let Some(out) = as_token(true, true, vec![]) else { return };
    let shown_on = last(&out);
    assert_eq!(count(shown_on, SELECTORS[0]), 1, "{out}");
    assert_eq!(count(shown_on, SELECTORS[4]), 0, "no pill beside the switch: {out}");
    assert_eq!(count(shown_on, SELECTORS[5]), 0, "no Turn off button: {out}");
    assert!(shown(shown_on).iter().any(|t| t == "On"), "{out}");
    assert!(!shown(shown_on).iter().any(|t| t == "Turn off"), "{out}");

    let Some(out) = as_token(true, true, vec![json!({"click": "[data-actions-off]"})]) else { return };
    let asking = last(&out);
    assert_eq!(count(asking, SELECTORS[6]), 1, "{out}");
    assert_eq!(count(asking, SELECTORS[3]), 1, "the switch waits while its question is open: {out}");
}

#[test]
fn off_with_a_passkey_session_is_a_live_switch_and_flipping_it_asks_first() {
    let Some(out) = as_session(vec![json!({"click": "[data-actions-on]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], SELECTORS[1]), 1, "{out}");
    assert_eq!(count(&frames[0], SELECTORS[5]), 0, "no Turn on button: {out}");
    assert_eq!(count(last(&out), SELECTORS[6]), 1, "{out}");
}

#[test]
fn a_switch_the_page_cannot_flip_is_greyed_out_with_its_word() {
    for version in [true, false] {
        let Some(out) = as_token(false, version, vec![]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, SELECTORS[2]), 1, "version {version}: {out}");
        assert!(shown(frame).iter().any(|t| t == "Off"), "{out}");
    }
}
