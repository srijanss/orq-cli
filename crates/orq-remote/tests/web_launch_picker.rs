//! With remote actions on, a pending feature's page offers "Start with…" beside Start, the popup's launch
//! picker: it reads the launch profiles, preselects the implementer and reviewer defaults, asks with the
//! picked pair and posts them to the start route; a refusal shows the server's reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 6] = ["[data-start-with]", "[data-start]", "[data-confirm]", "[data-cancel]", "[data-launch-implementer]", "[data-launch-reviewer]"];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger(actions: bool) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("sketch", "draft"), feature("next-one", "pending"), feature("done-one", "completed")], "remote_actions": actions}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn page(feature: &str, actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), runs(), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/features/{feature}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn profiles() -> Value {
    json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}})
}

#[test]
fn only_a_pending_feature_offers_start_with_and_only_with_actions_on() {
    let Some(out) = page("next-one", false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-start-with]"), 0, "{out}");
    let Some(out) = page("next-one", true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-start-with]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-start]"), 1, "Start with the defaults stays: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Start with…"), "{out}");
    for feature in ["sketch", "done-one"] {
        let Some(out) = page(feature, true, vec![], vec![]) else { return };
        assert_eq!(count(last(&out), "[data-start-with]"), 0, "{feature}: {out}");
    }
}

#[test]
fn the_picker_preselects_the_defaults_asks_with_the_picked_pair_and_posts_it() {
    let started = json!({"status": 202, "json": {"run": "run-7"}});
    let steps = vec![
        json!({"click": "[data-start-with]"}),
        json!({"set": "[data-launch-reviewer]", "value": "haiku"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = page("next-one", true, vec![profiles(), started], steps) else { return };
    let read = &out["requests"].as_array().unwrap().iter().find(|r| r["url"] == "/api/projects/alpha/launch-profiles").cloned().expect("the profiles are read");
    assert_eq!(read["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Start next-one with opus as implementer and sonnet as reviewer?"), "the defaults are preselected: {out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Start next-one with opus as implementer and haiku as reviewer?"), "{out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Yes, start"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/next-one/runs");
    assert_eq!(sent[0]["body"], r#"{"implementer":"opus","reviewer":"haiku"}"#);
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Started run run-7."), "{out}");
}

#[test]
fn a_refusal_shows_the_reason_no_profiles_says_so_and_cancel_sends_nothing() {
    let refused = json!({"status": 409, "json": {"error": "the profile haiku is not defined in the model profiles"}});
    let Some(out) = page("next-one", true, vec![profiles(), refused], vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "the profile haiku is not defined in the model profiles"), "{out}");
    assert_eq!(count(last(&out), "[data-start-with]"), 1, "the control is offered again: {out}");

    let none = json!({"status": 200, "json": {"profiles": [], "implementer": null, "reviewer": null}});
    let Some(out) = page("next-one", true, vec![none], vec![json!({"click": "[data-start-with]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "No model profiles are defined."), "{out}");
    assert_eq!(count(last(&out), "[data-launch-implementer]"), 0, "{out}");

    let Some(out) = page("next-one", true, vec![profiles()], vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-cancel]"})]) else { return };
    assert!(posts(&out).is_empty(), "cancelling sends nothing: {out}");
}
