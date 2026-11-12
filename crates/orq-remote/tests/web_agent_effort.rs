//! The agent editor offers an Effort select under Model when the chosen harness lists effort levels: "Not set"
//! and the harness's levels, starting on the agent's own effort. Save asks with the effort in its question and
//! sends it (null when not set). Switching to another harness keeps the effort if that harness takes it, else
//! falls back to not set; a harness that lists no levels shows no select and sends no effort, so the server keeps it.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 4] = ["[data-agent-effort]", "[data-agent-effort] option", "[data-agent-effort] option[value=\"\"]:checked", "[data-confirm]"];

fn run(steps: Vec<Value>, effort: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let implementer = json!({"harness": "claude", "model": "sonnet", "effort": effort});
    let settings = json!({"status": 200, "json": {"settings": {"implementer": implementer, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
    let options = json!({"status": 200, "json": {"options": [
        {"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "medium", "high", "xhigh", "max"]},
        {"harness": "pi", "models": ["gpt-5"], "efforts": ["off", "minimal", "low", "medium", "high", "xhigh", "max"]},
        {"harness": "codex", "models": ["o3"]}
    ]}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let mut all = vec![json!({"submit": TOKEN}), json!({"click": "[data-agent-edit=\"implementer\"]"})];
    all.extend(steps);
    let scenario = json!({"url": "/settings", "responses": [settings.clone(), options, ok, settings], "steps": all, "selectors": SELECTORS});
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn sent(out: &Value) -> Value {
    let posts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    serde_json::from_str(posts[0]["body"].as_str().unwrap()).unwrap()
}

fn save() -> Vec<Value> {
    vec![json!({"click": "[data-agent-save]"}), json!({"click": "[data-confirm]"})]
}

#[test]
fn the_effort_select_offers_not_set_and_the_harness_levels_and_save_sends_the_chosen_one() {
    let mut steps = vec![json!({"set": "[data-agent-effort]", "value": "max"}), json!({"click": "[data-agent-save]"})];
    steps.push(json!({"click": "[data-confirm]"}));
    let Some(out) = run(steps, json!("high")) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[0]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 6, "Not set and five levels: {out}");
    assert!(shown(&frames[2]).iter().any(|t| t == "Not set"), "{out}");
    assert!(shown(&frames[4]).iter().any(|t| t == "Save implementer: claude sonnet, effort max?"), "{out}");
    assert_eq!(sent(&out), json!({"implementer": {"harness": "claude", "model": "sonnet", "effort": "max"}, "version": "v-1"}));
}

#[test]
fn not_set_sends_null() {
    let mut steps = vec![json!({"set": "[data-agent-effort]", "value": ""})];
    steps.extend(save());
    let Some(out) = run(steps, json!("high")) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(shown(&frames[4]).iter().any(|t| t == "Save implementer: claude sonnet, effort not set?"), "{out}");
    assert_eq!(sent(&out), json!({"implementer": {"harness": "claude", "model": "sonnet", "effort": null}, "version": "v-1"}));
}

#[test]
fn a_harness_that_takes_the_effort_keeps_it_and_one_with_no_levels_shows_no_select_and_sends_none() {
    let mut steps = vec![json!({"set": "[data-agent-harness]", "value": "pi"})];
    steps.extend(save());
    let Some(out) = run(steps, json!("xhigh")) else { return };
    assert_eq!(sent(&out), json!({"implementer": {"harness": "pi", "model": "gpt-5", "effort": "xhigh"}, "version": "v-1"}));

    let back = vec![json!({"set": "[data-agent-harness]", "value": "pi"}), json!({"set": "[data-agent-effort]", "value": "minimal"}), json!({"set": "[data-agent-harness]", "value": "claude"})];
    let Some(out) = run(back, json!("high")) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[4], SELECTORS[2]), 0, "pi takes minimal: {out}");
    assert_eq!(count(frames.last().unwrap(), SELECTORS[2]), 1, "claude lacks minimal, so it falls back to not set: {out}");

    let mut steps = vec![json!({"set": "[data-agent-harness]", "value": "codex"})];
    let Some(shown_codex) = run(steps.clone(), json!("high")) else { return };
    assert_eq!(count(shown_codex["frames"].as_array().unwrap().last().unwrap(), SELECTORS[0]), 0, "{shown_codex}");
    steps.extend(save());
    let Some(out) = run(steps, json!("high")) else { return };
    assert!(shown(&out["frames"][4]).iter().any(|t| t == "Save implementer: codex o3?"), "{out}");
    assert_eq!(sent(&out), json!({"implementer": {"harness": "codex", "model": "o3"}, "version": "v-1"}));
}
