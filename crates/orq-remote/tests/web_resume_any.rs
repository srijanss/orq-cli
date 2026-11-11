//! With remote actions on, Resume appears on paused, failed and blocked runs and Review again on
//! needs_review runs, on the row and in the run detail header. Each first asks the server's resume check
//! and then "Resume {run} from {next}?"; when the check refuses, its reason is shown and nothing is asked.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-resume]", "[data-confirm]", "[data-cancel]"];

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
    let rows = [("r-fail", "failed"), ("r-paused", "paused"), ("r-blocked", "blocked"), ("r-review", "needs_review"), ("r-run", "running"), ("r-done", "completed")];
    json!({"status": 200, "json": {"runs": rows.iter().map(|(id, state)| one(id, state)).collect::<Vec<_>>(), "next_cursor": null, "total": 6, "running": 1, "remote_actions": actions}})
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
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == method && r["url"].as_str().unwrap().contains("/resume")).cloned().collect()
}

fn checked(next: &str, run: &str) -> Value {
    json!({"status": 200, "json": {"run": run, "next": next}})
}

#[test]
fn paused_failed_blocked_and_needs_review_runs_get_resume_only_with_actions_on() {
    let Some(out) = runs_page(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-resume]"), 0, "{out}");
    let Some(out) = runs_page(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-resume]"), 4, "{out}");
    let shown = texts(last(&out));
    assert_eq!(shown.iter().filter(|t| *t == "Resume").count(), 3, "{shown:?}");
    assert_eq!(shown.iter().filter(|t| *t == "Review again").count(), 1, "{shown:?}");
    assert!(sent(&out, "GET").is_empty() && sent(&out, "POST").is_empty(), "showing it asks nothing: {out}");
}

#[test]
fn resume_checks_first_then_asks_from_where_and_posts_to_the_resume_route() {
    let ok = json!({"status": 202, "json": {"run": "r-fail"}});
    let Some(out) = runs_page(true, vec![checked("where it stopped", "r-fail"), ok], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Resume r-fail from where it stopped?"), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Yes, resume"), "{out}");
    let checks = sent(&out, "GET");
    assert_eq!(checks.len(), 1, "{out}");
    assert_eq!(checks[0]["url"], "/api/projects/alpha/runs/r-fail/resume-check");
    assert_eq!(checks[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let posts = sent(&out, "POST");
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-fail/resume");
    assert_eq!(posts[0]["body"], "{}");
}

#[test]
fn a_refused_check_shows_the_reason_and_asks_nothing() {
    let refused = json!({"status": 409, "json": {"error": "r-fail has no checkpoint to resume from"}});
    let Some(out) = runs_page(true, vec![refused], vec![json!({"click": "[data-resume]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "r-fail has no checkpoint to resume from"), "{out}");
    assert_eq!(count(end, "[data-confirm]"), 0, "{out}");
    assert!(sent(&out, "POST").is_empty(), "{out}");
}

#[test]
fn the_run_detail_header_offers_review_again_for_a_needs_review_run() {
    let ok = json!({"status": 202, "json": {"run": "r-review"}});
    let Some(out) = detail("r-review", "needs_review", vec![checked("where it stopped", "r-review"), ok], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Review again"), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Resume r-review from where it stopped?"), "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Yes, review again"), "{out}");
    assert_eq!(sent(&out, "POST").first().map(|r| r["url"].clone()), Some(json!("/api/projects/alpha/runs/r-review/resume")), "{out}");
    let Some(out) = detail("r-blocked", "blocked", vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-resume]"), 1, "{out}");
}
