//! With remote actions on, a marked pending feature counts toward one "Start N" button, the popup's launch
//! marked. It opens the launch picker, asks with the picked pair, then sends one start request per marked
//! feature in dependency order with those profiles, and reports each run started or refused.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = ["[data-mark]", "[data-start-marked]", "[data-promote-marked]", "[data-confirm]", "[data-cancel]", "[data-marked-result]", "[data-launch-reviewer]"];

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

fn ledger() -> Value {
    let feature = |name: &str, status: &str, deps: &[&str]| json!({"name": name, "description": null, "status": status, "depends_on": deps, "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("a-base", "pending", &[]), feature("b-top", "pending", &["c-mid"]), feature("c-mid", "pending", &["a-base"]), feature("d-draft", "draft", &[]), feature("e-done", "completed", &[])], "remote_actions": true}})
}

fn profiles() -> Value {
    json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}})
}

fn list(extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(), projects()];
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
fn pending_rows_can_be_marked_and_start_counts_only_the_marked_pending_features() {
    let Some(out) = list(vec![], vec![mark("d-draft"), mark("a-base")]) else { return };
    assert_eq!(count(&out["frames"][1], "[data-mark]"), 4, "a checkbox on each draft and pending row: {out}");
    assert_eq!(count(&out["frames"][1], "[data-start-marked]"), 0, "{out}");
    assert_eq!(count(&out["frames"][2], "[data-start-marked]"), 0, "a marked draft is promoted, not started: {out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Start 1"), "{out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Promote 1"), "{out}");
    assert!(posts(&out).is_empty(), "marking sends nothing: {out}");
}

#[test]
fn a_confirmed_start_marked_posts_the_picked_profiles_once_per_feature_in_dependency_order_and_reports_each() {
    let started = json!({"status": 202, "json": {"run": "run-1"}});
    let refused = json!({"status": 409, "json": {"error": "feature c-mid is waiting on a-base"}});
    let later = json!({"status": 202, "json": {"run": "run-3"}});
    let steps = vec![
        mark("b-top"),
        mark("a-base"),
        mark("c-mid"),
        json!({"click": "[data-start-marked]"}),
        json!({"set": "[data-launch-reviewer]", "value": "haiku"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = list(vec![profiles(), started, refused, later, ledger(), projects()], steps) else { return };
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Start 3 features: a-base, c-mid, b-top with opus as implementer and haiku as reviewer?"), "{out}");
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Yes, start"), "{out}");
    let sent = posts(&out);
    let urls: Vec<&str> = sent.iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["/api/projects/alpha/features/a-base/runs", "/api/projects/alpha/features/c-mid/runs", "/api/projects/alpha/features/b-top/runs"], "dependency order, one each: {out}");
    for request in &sent {
        assert_eq!(request["body"], r#"{"implementer":"opus","reviewer":"haiku"}"#);
        assert_eq!(request["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    }
    let shown = texts(last(&out));
    assert!(shown.iter().any(|t| t == "Started run run-1 for a-base."), "{shown:?}");
    assert!(shown.iter().any(|t| t == "c-mid: feature c-mid is waiting on a-base"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "Started run run-3 for b-top."), "{shown:?}");
    assert_eq!(count(last(&out), "[data-marked-result]"), 3, "{out}");
    assert_eq!(count(last(&out), "[data-start-marked]"), 0, "the marks are cleared: {out}");
}

#[test]
fn cancelling_sends_nothing_no_profiles_says_so_and_a_refused_sign_in_stops_the_rest() {
    let steps = vec![mark("a-base"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-cancel]"})];
    let Some(out) = list(vec![profiles()], steps) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-start-marked]"), 1, "the marks stay after a cancel: {out}");

    let none = json!({"status": 200, "json": {"profiles": [], "implementer": null, "reviewer": null}});
    let Some(out) = list(vec![none], vec![mark("a-base"), json!({"click": "[data-start-marked]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "No model profiles are defined."), "{out}");

    let steps = vec![mark("a-base"), mark("c-mid"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = list(vec![profiles(), json!({"status": 401})], steps) else { return };
    assert_eq!(posts(&out).len(), 1, "nothing more is sent once signed out: {out}");
}

/// Start N offers Project default like Start with…: preselected when no profile has the project's
/// agents, it starts each marked feature with no profiles named.
#[test]
fn start_marked_with_project_default_starts_each_feature_with_no_profiles_named() {
    let offer = json!({"status": 200, "json": {"profiles": ["haiku", "opus"], "implementer": null, "reviewer": null, "project_default": true}});
    let started = |run: &str| json!({"status": 202, "json": {"run": run}});
    let steps = vec![mark("a-base"), mark("c-mid"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = list(vec![offer, started("run-1"), started("run-2"), ledger(), projects()], steps) else { return };
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Start 2 features: a-base, c-mid with the project's default agents?"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 2, "{out}");
    for request in &sent {
        assert_eq!(request["body"], "{}", "{out}");
    }
}
