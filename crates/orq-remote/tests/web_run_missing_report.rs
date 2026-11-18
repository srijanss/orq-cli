//! A run that ended without its review report says so on its page, with why, and offers Build it now when the
//! server allows actions; Yes posts to the build-report route and the note says the report was built.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-missing-report]", "[data-build-report]", "[data-confirm]"];
const REASON: &str = "cannot open the review index: orq.db has schema version 9, newer than this Orq understands (8)";

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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ended(missing: bool) -> Value {
    let mut run = json!({"run_id": "r-done", "feature": "admin-alert-dispatch", "state": "completed", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    if missing {
        run["missing_report"] = json!({"head_sha": "49485ef0000000000000000000000000000000aa", "reason": REASON});
    }
    run
}

fn detail(run: Value, actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let body = json!({"run": run, "events": [], "next_cursor": null, "remote_actions": actions});
    let mut responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    self::run(json!({"url": "/projects/alpha/orchestrations/r-done", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_run_without_its_report_says_why_and_offers_build_it_now() {
    let Some(out) = detail(ended(true), true, vec![], vec![]) else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-missing-report]"), 1, "{out}");
    assert!(texts(end).iter().any(|t| t == &format!("Review report missing: {REASON}")), "{:?}", texts(end));
    assert_eq!(count(end, "[data-build-report]"), 1, "{out}");
}

#[test]
fn build_it_now_asks_then_posts_to_the_build_report_route() {
    let ok = json!({"status": 202, "json": {"run": "r-done"}});
    let Some(out) = detail(ended(true), true, vec![ok], vec![json!({"click": "[data-build-report]"}), json!({"click": "[data-confirm]"})]) else { return };
    let posted: Vec<_> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST" && r["url"].as_str().unwrap().ends_with("/api/projects/alpha/runs/r-done/build-report")).collect();
    assert_eq!(posted.len(), 1, "{out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Report built."), "{:?}", texts(last(&out)));
}

#[test]
fn without_actions_the_note_stays_and_nothing_is_offered() {
    let Some(out) = detail(ended(true), false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-missing-report]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-build-report]"), 0, "{out}");
}

#[test]
fn a_run_with_its_report_shows_no_note() {
    let Some(out) = detail(ended(false), true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-missing-report]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-build-report]"), 0, "{out}");
}
