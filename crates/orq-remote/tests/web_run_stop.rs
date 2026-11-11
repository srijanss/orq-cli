//! With remote actions on, a running or pause_requested run gets Stop, on its row and in the run detail
//! header. It asks "Stop {run}? Its process ends and it can be resumed later." first; yes posts to
//! …/runs/{run}/stop; Cancel sends nothing; a refusal shows the server's reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-stop]", "[data-confirm]", "[data-cancel]"];

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

fn one(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"})
}

fn runs(actions: bool) -> Value {
    let rows = [("r-run", "running"), ("r-ask", "pause_requested"), ("r-paused", "paused"), ("r-fail", "failed"), ("r-done", "completed")];
    json!({"status": 200, "json": {"runs": rows.iter().map(|(id, state)| one(id, state)).collect::<Vec<_>>(), "next_cursor": null, "total": 5, "running": 1, "remote_actions": actions}})
}

fn runs_page(actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![runs(actions), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn detail(state: &str, actions: Option<bool>, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut body = json!({"run": one("r-run", state), "events": [], "next_cursor": null});
    if let Some(on) = actions {
        body["remote_actions"] = json!(on);
    }
    let mut responses = vec![json!({"status": 200, "json": body}), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations/r-run", "responses": responses, "steps": all, "selectors": SELECTORS}))
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
fn only_running_and_pause_requested_rows_get_stop_and_only_with_actions_on() {
    let Some(out) = runs_page(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-stop]"), 0, "{out}");
    let Some(out) = runs_page(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-stop]"), 2, "{out}");
    assert!(posts(&out).is_empty(), "showing it sends nothing");
}

#[test]
fn stop_asks_by_name_first_then_posts_to_the_runs_stop_route() {
    let ok = json!({"status": 202, "json": {"run": "r-run"}});
    let Some(out) = runs_page(true, vec![ok], vec![json!({"click": "[data-stop]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Stop r-run? Its process ends and it can be resumed later."), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Yes, stop"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/runs/r-run/stop");
    assert_eq!(sent[0]["body"], "{}");
}

#[test]
fn cancel_sends_nothing_and_a_refusal_shows_the_servers_reason() {
    let Some(out) = runs_page(true, vec![], vec![json!({"click": "[data-stop]"}), json!({"click": "[data-cancel]"})]) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    let refused = json!({"status": 409, "json": {"error": "r-run is completed; only a running run can be stopped"}});
    let Some(out) = runs_page(true, vec![refused], vec![json!({"click": "[data-stop]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "r-run is completed; only a running run can be stopped"), "{out}");
}

#[test]
fn the_run_detail_header_offers_stop_when_the_server_says_actions_are_on() {
    let Some(out) = detail("running", None, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-stop]"), 0, "{out}");
    let Some(out) = detail("completed", Some(true), vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-stop]"), 0, "{out}");
    let ok = json!({"status": 202, "json": {"run": "r-run"}});
    let Some(out) = detail("pause_requested", Some(true), vec![ok], vec![json!({"click": "[data-stop]"}), json!({"click": "[data-confirm]"})]) else { return };
    let sent = posts(&out);
    assert_eq!(sent.first().map(|r| r["url"].clone()), Some(json!("/api/projects/alpha/runs/r-run/stop")), "{out}");
}
