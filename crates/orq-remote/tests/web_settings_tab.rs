//! The global Settings page: the run settings from `/api/settings`, read-only and drawn as text.
//! Nothing on it can change a setting.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings() -> Value {
    json!({"status": 200, "json": {"settings": {
        "implementer": {"harness": "claude", "model": "opus", "effort": "high"},
        "reviewer": {"harness": "pi", "model": "<b>gpt</b>", "effort": null},
        "models": [{"name": "opus", "harness": "claude", "model": "opus", "effort": null}]
    }}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_settings_page_reads_the_global_endpoint_once_and_lists_each_agent_and_model() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["requests"][0]["url"], "/api/settings", "{out}");
    assert_eq!(out["requests"][0]["method"], "GET", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let t = shown(&out["frames"][1]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("Settings") < at("Implementer") && at("Implementer") < at("Reviewer") && at("Reviewer") < at("Models"), "{t:?}");
    for text in ["claude", "opus", "high", "pi", "<b>gpt</b>"] {
        assert!(t.iter().any(|x| x == text), "{text} missing: {t:?}");
    }
    assert!(!t.iter().any(|x| x.contains("not available yet")), "{t:?}");
}

#[test]
fn a_setting_with_no_value_shows_a_dash_and_says_it_is_not_set() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    let t = shown(&out["frames"][1]);
    assert!(t.iter().any(|x| x == "—"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("not set")), "{t:?}");
    let Some(out) = run("/settings", json!([{"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": []}}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|x| x.contains("No models configured")), "{out}");
}

#[test]
fn nothing_on_the_page_can_change_a_setting() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1]["settled"];
    assert_eq!(frame["inputs"], json!([]), "{out}");
    assert_eq!(frame["buttons"], json!([]), "{out}");
    assert_eq!(frame["links"], json!([]), "{out}");
}

#[test]
fn unusable_settings_draw_nothing_and_a_refused_token_asks_again() {
    let Some(out) = run("/settings", json!([{"status": 200, "json": {"settings": "x"}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(!shown(&out["frames"][1]).iter().any(|x| x == "Implementer"), "{out}");
    let Some(out) = run("/settings", json!([{"status": 401}]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
}
