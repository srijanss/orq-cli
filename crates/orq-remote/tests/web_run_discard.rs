//! With remote actions on, Discard appears on paused, failed, blocked and needs_review runs and Clear on
//! merged ones, on the row and in the run detail header. Each first asks the server what the discard would
//! take with the branch, then asks the question with that warning; Yes posts to the discard route.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-discard]", "[data-confirm]", "[data-cancel]"];

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
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == method && r["url"].as_str().unwrap().contains("/discard")).cloned().collect()
}

fn warned(run: &str, warning: &str) -> Value {
    json!({"status": 200, "json": {"run": run, "warning": warning}})
}

#[test]
fn discard_and_clear_ask_the_warning_first_then_post_to_the_discard_route() {
    let Some(out) = runs_page(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-discard]"), 0, "{out}");
    let Some(out) = runs_page(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-discard]"), 5, "{out}");
    let shown = texts(last(&out));
    assert_eq!(shown.iter().filter(|t| *t == "Discard").count(), 4, "{shown:?}");
    assert_eq!(shown.iter().filter(|t| *t == "Clear").count(), 1, "{shown:?}");
    assert!(sent(&out, "GET").is_empty() && sent(&out, "POST").is_empty(), "showing it asks nothing: {out}");

    let ok = json!({"status": 202, "json": {"run": "r-fail", "note": " f-r-fail is pending again.", "cleared": false}});
    let warning = warned("r-fail", "its branch orq/r-fail goes with 1 commit not on the target branch");
    let Some(out) = runs_page(true, vec![warning, ok], vec![json!({"click": "[data-discard]"}), json!({"click": "[data-confirm]"})]) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Discard r-fail? Logs and a clean worktree go, and its branch orq/r-fail goes with 1 commit not on the target branch; uncommitted work is kept."), "{asked:?}");
    assert!(asked.iter().any(|t| t == "Yes, discard"), "{asked:?}");
    let checks = sent(&out, "GET");
    assert_eq!(checks.len(), 1, "{out}");
    assert_eq!(checks[0]["url"], "/api/projects/alpha/runs/r-fail/discard-warning");
    assert_eq!(checks[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let posts = sent(&out, "POST");
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-fail/discard");
    assert_eq!(posts[0]["body"], "{}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Discarded r-fail. f-r-fail is pending again."), "{out}");
}

#[test]
fn without_a_warning_the_branch_goes_and_clear_says_the_features_stay() {
    let Some(out) = detail("r-paused", "paused", vec![warned("r-paused", "")], vec![json!({"click": "[data-discard]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "Discard r-paused? Logs, a clean worktree and its branch go; uncommitted work is kept."), "{out}");

    let ok = json!({"status": 202, "json": {"run": "r-merged", "note": " Its work is on the target branch; its features stay as the ledger has them.", "cleared": true}});
    let Some(out) = detail("r-merged", "merged", vec![warned("r-merged", ""), ok], vec![json!({"click": "[data-discard]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Clear"), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Clear r-merged? Its work is on the target; its features stay, its record and logs go."), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Yes, clear"), "{out}");
    assert_eq!(sent(&out, "POST").first().map(|r| r["url"].clone()), Some(json!("/api/projects/alpha/runs/r-merged/discard")), "{out}");
}

#[test]
fn a_refused_warning_shows_the_reason_and_asks_nothing() {
    let refused = json!({"status": 409, "json": {"error": "r-fail is running"}});
    let Some(out) = runs_page(true, vec![refused], vec![json!({"click": "[data-discard]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "r-fail is running"), "{out}");
    assert_eq!(count(end, "[data-confirm]"), 0, "{out}");
    assert!(sent(&out, "POST").is_empty(), "{out}");
}
