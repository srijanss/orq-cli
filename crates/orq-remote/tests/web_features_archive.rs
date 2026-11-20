//! The Features page's archive: an "Archived (N)" filter lists the archived features read-only with a search, an
//! archived feature's page is read-only, and "Archive completed (M)" shows only with actions on and over 200
//! completed features; it asks with the dry-run counts, then posts the real archive.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = ["[data-archived-toggle]", "[data-archived-search]", "[data-archived-row]", "[data-archive-completed]", "[data-confirm]", "[data-promote]", "[data-start]"];

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

fn ledger(completed: usize, actions: bool) -> Value {
    let mut features: Vec<Value> = (0..completed).map(|i| json!({"name": format!("done-{i}"), "description": null, "status": "completed", "depends_on": [], "recorded_at": null})).collect();
    features.push(json!({"name": "next-one", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}));
    json!({"status": 200, "json": {"features": features, "remote_actions": actions}})
}

fn archived() -> Value {
    let entry = |name: &str| json!({"name": name, "description": "an old one", "status": "completed", "depends_on": [], "recorded_at": null, "archived_at": "2026-11-20T10:15:00Z"});
    json!({"status": 200, "json": {"archived": [entry("old-alpha"), entry("old-beta"), entry("old-gamma")]}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn page(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    // The archive read is scripted apart from the other replies; the archive action's reply has a count, not a list.
    let (archived, responses): (Vec<Value>, Vec<Value>) = responses.into_iter().partition(|r| r["json"]["archived"].is_array() || r["archive_read"] == true);
    run(json!({"url": url, "responses": responses, "archived": archived, "steps": all, "selectors": SELECTORS}))
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
fn the_archived_filter_lists_the_archived_features_read_only_and_searches_them() {
    let responses = vec![ledger(2, true), projects(), archived()];
    let Some(out) = page("/projects/alpha/features", responses, vec![json!({"click": "[data-archived-toggle]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "Archived (3)"), "{out}");
    assert_eq!(count(end, "[data-archived-row]"), 3, "{out}");
    assert_eq!(count(end, "[data-promote]") + count(end, "[data-start]"), 0, "archived features are read-only: {out}");
    assert_eq!(count(end, "[data-archive-completed]"), 0, "not over 200 completed: {out}");
    assert_eq!(count(end, "[data-archived-search]"), 1, "{out}");
}

#[test]
fn an_archived_features_page_is_read_only_and_a_missing_one_is_not_found() {
    let responses = vec![ledger(2, true), runs(), projects(), archived()];
    let Some(out) = page("/projects/alpha/features/old-beta", responses, vec![]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t.contains("old-beta")), "{out}");
    assert!(texts(end).iter().any(|t| t.contains("Archived")), "{out}");
    assert_eq!(count(end, "[data-promote]") + count(end, "[data-start]"), 0, "{out}");
    assert!(!texts(end).iter().any(|t| t == "Not found"), "{out}");

    let responses = vec![ledger(2, true), runs(), projects(), archived()];
    let Some(out) = page("/projects/alpha/features/nothing-here", responses, vec![]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "Not found"), "{out}");
}

/// A failed archive read is told apart from a slow one: the feature page shows an error with a retry, not "Loading" for ever,
/// and a refused token asks for the token again.
#[test]
fn a_failed_archive_read_shows_an_error_and_a_refused_token_signs_out() {
    for failure in [json!({"status": 500, "json": {"error": "boom"}, "archive_read": true}), json!({"reject": true, "archive_read": true})] {
        let responses = vec![ledger(2, true), runs(), projects(), failure];
        let Some(out) = page("/projects/alpha/features/old-beta", responses, vec![]) else { return };
        let end = last(&out);
        assert!(!texts(end).iter().any(|t| t.contains("Loading the feature")), "{out}");
        assert!(texts(end).iter().any(|t| t.contains("could not be read")), "{out}");
        assert!(!texts(end).iter().any(|t| t == "Not found"), "{out}");
    }

    let responses = vec![ledger(2, true), runs(), projects(), json!({"status": 401, "json": {}, "archive_read": true})];
    let Some(out) = page("/projects/alpha/features/old-beta", responses, vec![]) else { return };
    assert_eq!(last(&out)["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
}

#[test]
fn archive_completed_shows_over_200_completed_with_actions_on_and_asks_with_the_dry_run_counts() {
    let dry = json!({"status": 200, "json": {"archived": 101, "dry_run": true, "stripped": [{"feature": "next-one", "names": ["done-0"]}]}});
    let done = json!({"status": 200, "json": {"archived": 101, "dry_run": false, "stripped": []}});
    let responses = vec![ledger(201, true), projects(), archived(), dry, done];
    let steps = vec![json!({"click": "[data-archive-completed]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = page("/projects/alpha/features", responses, steps) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t.contains("Archive 101 features?")), "{asked:?}");
    let before = texts(&out["frames"][1]);
    assert!(before.iter().any(|t| t == "Archive completed (201)"), "{before:?}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 2, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/archive");
    assert_eq!(sent[0]["body"], r#"{"dry_run":true}"#);
    assert_eq!(sent[1]["body"], r#"{"dry_run":false}"#);

    let Some(out) = page("/projects/alpha/features", vec![ledger(201, false), projects(), archived()], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-archive-completed]"), 0, "actions off: {out}");
    let Some(out) = page("/projects/alpha/features", vec![ledger(200, true), projects(), archived()], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-archive-completed]"), 0, "exactly 200 is not over: {out}");
}
