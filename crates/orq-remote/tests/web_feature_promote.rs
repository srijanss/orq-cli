//! With remote actions on, a draft feature's page offers Promote, as the popup does. It asks "Promote {name}?"
//! first; Yes posts to the promote route and says the feature is pending, and a refusal shows the reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 4] = ["[data-promote]", "[data-start]", "[data-confirm]", "[data-cancel]"];

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

#[test]
fn only_a_draft_offers_promote_and_only_with_actions_on() {
    let Some(out) = page("sketch", false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-promote]"), 0, "{out}");
    let Some(out) = page("sketch", true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-promote]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-start]"), 0, "a draft cannot be started yet: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Promote"), "{out}");
    for feature in ["next-one", "done-one"] {
        let Some(out) = page(feature, true, vec![], vec![]) else { return };
        assert_eq!(count(last(&out), "[data-promote]"), 0, "{feature}: {out}");
    }
}

#[test]
fn a_confirmed_promote_posts_and_says_the_feature_is_pending_and_a_refusal_shows_the_reason() {
    let ok = json!({"status": 200, "json": {"feature": "sketch", "status": "pending"}});
    let Some(out) = page("sketch", true, vec![ok], vec![json!({"click": "[data-promote]"}), json!({"click": "[data-confirm]"})]) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Promote sketch?"), "{asked:?}");
    assert!(asked.iter().any(|t| t == "Yes, promote"), "{asked:?}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/sketch/promote");
    assert_eq!(sent[0]["body"], "{}");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Promoted sketch; it is pending."), "{out}");

    let refused = json!({"status": 409, "json": {"error": "ledger is locked by another writer; nothing was written, try again"}});
    let Some(out) = page("sketch", true, vec![refused], vec![json!({"click": "[data-promote]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "ledger is locked by another writer; nothing was written, try again"), "{out}");
    assert_eq!(count(last(&out), "[data-promote]"), 1, "the control is offered again: {out}");

    let Some(out) = page("sketch", true, vec![], vec![json!({"click": "[data-promote]"}), json!({"click": "[data-cancel]"})]) else { return };
    assert!(posts(&out).is_empty(), "cancelling sends nothing: {out}");
}
