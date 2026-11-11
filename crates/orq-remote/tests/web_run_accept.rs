//! With remote actions on, Accept appears on failed and needs_review runs, on the row and in the run detail
//! header. It asks first; Yes posts to the accept route, whose tests run on the server, so the done note says
//! the result shows on the run once they finish.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-accept]", "[data-confirm]", "[data-cancel]"];

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
    let rows = [("r-fail", "failed"), ("r-paused", "paused"), ("r-blocked", "blocked"), ("r-review", "needs_review"), ("r-merged", "merged"), ("r-run", "running"), ("r-done", "completed")];
    json!({"status": 200, "json": {"runs": rows.iter().map(|(id, state)| one(id, state)).collect::<Vec<_>>(), "next_cursor": null, "total": 7, "running": 1, "remote_actions": actions}})
}

fn runs_page(actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![runs(actions), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn detail(id: &str, state: &str, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let body = json!({"run": one(id, state), "events": [], "next_cursor": null, "remote_actions": true});
    // An empty run follows its events live; the follow gets its own held stream.
    let mut responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
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

fn sent(out: &Value, method: &str) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == method && r["url"].as_str().unwrap().contains("/accept")).cloned().collect()
}

#[test]
fn accept_shows_on_failed_and_review_waiting_runs_and_posts_after_asking() {
    let Some(out) = runs_page(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-accept]"), 0, "{out}");
    let Some(out) = runs_page(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-accept]"), 2, "only the failed and the needs_review run: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Accept"), "{out}");

    let ok = json!({"status": 202, "json": {"run": "r-fail"}});
    let Some(out) = runs_page(true, vec![ok], vec![json!({"click": "[data-accept]"}), json!({"click": "[data-confirm]"})]) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Accept r-fail? The tests run in its worktree; if they pass it is marked review ready."), "{asked:?}");
    assert!(asked.iter().any(|t| t == "Yes, accept"), "{asked:?}");
    assert!(sent(&out, "GET").is_empty(), "accept asks the server nothing first: {out}");
    let posts = sent(&out, "POST");
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-fail/accept");
    assert_eq!(posts[0]["body"], "{}");
    assert_eq!(posts[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Accepting r-fail: its tests are running. It shows as review ready when they pass."), "{out}");
}

#[test]
fn a_refused_accept_shows_the_reason_in_the_run_detail() {
    let refused = json!({"status": 409, "json": {"error": "r-review has uncommitted changes in its worktree; commit or discard them first"}});
    let Some(out) = detail("r-review", "needs_review", vec![refused], vec![json!({"click": "[data-accept]"}), json!({"click": "[data-confirm]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "r-review has uncommitted changes in its worktree; commit or discard them first"), "{out}");
    assert_eq!(count(end, "[data-accept]"), 1, "the control is offered again: {out}");
}
