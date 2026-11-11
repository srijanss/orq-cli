//! With remote actions on, the Features list puts a checkbox on each draft row and, once any are marked,
//! one "Promote N" button. It asks first, then sends one promote request per marked feature in ledger order
//! (there is no bulk route, so each is audited on its own) and reports each result.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = ["[data-mark]", "[data-promote-marked]", "[data-confirm]", "[data-cancel]", "[data-marked-result]"];

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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger(actions: bool, a_status: &str) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("a-draft", a_status), feature("b-pending", "pending"), feature("c-draft", "draft"), feature("d-draft", "draft")], "remote_actions": actions}})
}

fn list(actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions, "draft"), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/features", "responses": responses, "steps": all, "selectors": SELECTORS}))
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

fn mark(name: &str) -> Value {
    json!({"click": format!("[data-mark=\"{name}\"]")})
}

#[test]
fn only_draft_rows_can_be_marked_and_only_with_actions_on_and_the_button_counts_the_marks() {
    let Some(out) = list(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-mark]"), 0, "{out}");
    let Some(out) = list(true, vec![], vec![mark("d-draft"), mark("a-draft"), mark("d-draft")]) else { return };
    assert_eq!(count(&out["frames"][1], "[data-mark]"), 3, "a checkbox on each draft: {out}");
    assert_eq!(count(&out["frames"][1], "[data-promote-marked]"), 0, "nothing marked, no button: {out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Promote 1"), "{out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Promote 2"), "{out}");
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Promote 1"), "unmarking counts down: {out}");
    assert!(posts(&out).is_empty(), "marking sends nothing: {out}");
}

#[test]
fn a_confirmed_promote_marked_posts_once_per_feature_in_ledger_order_and_reports_each_result() {
    let ok = json!({"status": 200, "json": {"feature": "a-draft", "status": "pending"}});
    let refused = json!({"status": 409, "json": {"error": "feature d-draft: Pending to Pending is not allowed"}});
    let steps = vec![mark("d-draft"), mark("a-draft"), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = list(true, vec![ok, refused, ledger(true, "pending"), projects()], steps) else { return };
    let asked = texts(&out["frames"][4]);
    assert!(asked.iter().any(|t| t == "Promote 2 features: a-draft, d-draft?"), "{asked:?}");
    assert!(asked.iter().any(|t| t == "Yes, promote"), "{asked:?}");
    let sent = posts(&out);
    let urls: Vec<&str> = sent.iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["/api/projects/alpha/features/a-draft/promote", "/api/projects/alpha/features/d-draft/promote"], "ledger order, one each: {out}");
    for request in &sent {
        assert_eq!(request["body"], "{}");
        assert_eq!(request["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    }
    let shown = texts(last(&out));
    assert!(shown.iter().any(|t| t == "Promoted a-draft; it is pending."), "{shown:?}");
    assert!(shown.iter().any(|t| t == "d-draft: feature d-draft: Pending to Pending is not allowed"), "{shown:?}");
    assert_eq!(count(last(&out), "[data-marked-result]"), 2, "{out}");
    assert_eq!(count(last(&out), "[data-promote-marked]"), 0, "the marks are cleared: {out}");
}

#[test]
fn cancelling_sends_nothing_and_a_refused_sign_in_stops_the_rest() {
    let steps = vec![mark("a-draft"), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-cancel]"})];
    let Some(out) = list(true, vec![], steps) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-promote-marked]"), 1, "the marks stay after a cancel: {out}");

    let steps = vec![mark("a-draft"), mark("c-draft"), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = list(true, vec![json!({"status": 401})], steps) else { return };
    assert_eq!(posts(&out).len(), 1, "nothing more is sent once signed out: {out}");
}
