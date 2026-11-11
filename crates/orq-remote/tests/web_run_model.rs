//! With remote actions on, Switch model appears on paused runs, on the row and in the run detail header. It
//! reads the profiles the popup's picker offers, preselects the role's default, and asks "Switch the {role} of
//! {run} to {profile}?" with the picked ones; Yes posts that role and profile, and a refusal shows the reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = ["[data-model]", "[data-model-role]", "[data-model-profile]", "[data-confirm]", "[data-cancel]"];

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

fn sent(out: &Value, method: &str, part: &str) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == method && r["url"].as_str().unwrap().contains(part)).cloned().collect()
}

const PROFILES: &str = r#"{"profiles": ["opus", "kimi"], "implementer": "opus", "reviewer": "kimi"}"#;

fn profiles() -> Value {
    json!({"status": 200, "json": serde_json::from_str::<Value>(PROFILES).unwrap()})
}

#[test]
fn switch_model_shows_on_paused_runs_and_posts_the_picked_role_and_profile_after_asking() {
    let Some(out) = runs_page(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-model]"), 0, "{out}");
    let Some(out) = runs_page(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-model]"), 1, "only the paused run: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Switch model"), "{out}");

    let ok = json!({"status": 202, "json": {"run": "r-paused", "role": "reviewer", "profile": "opus"}});
    let steps = vec![
        json!({"click": "[data-model]"}),
        json!({"set": "[data-model-role]", "value": "reviewer"}),
        json!({"set": "[data-model-profile]", "value": "opus"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = runs_page(true, vec![profiles(), ok], steps) else { return };
    let gets = sent(&out, "GET", "/launch-profiles");
    assert_eq!(gets.len(), 1, "{out}");
    assert_eq!(gets[0]["url"], "/api/projects/alpha/launch-profiles");
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Switch the implementer of r-paused to opus? It takes effect when the run resumes."), "the role's default is preselected: {asked:?}");
    assert!(asked.iter().any(|t| t == "Yes, switch model"), "{asked:?}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Switch the reviewer of r-paused to kimi? It takes effect when the run resumes."), "{out}");
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Switch the reviewer of r-paused to opus? It takes effect when the run resumes."), "{out}");
    let posts = sent(&out, "POST", "/model");
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-paused/model");
    assert_eq!(serde_json::from_str::<Value>(posts[0]["body"].as_str().unwrap()).unwrap(), json!({"role": "reviewer", "profile": "opus"}));
    assert_eq!(posts[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][5]).iter().any(|t| t == "The reviewer of r-paused continues on opus when it resumes."), "{out}");
}

#[test]
fn a_refused_switch_or_unreadable_profiles_show_the_reason_in_the_run_detail() {
    let refused = json!({"status": 409, "json": {"error": "r-paused is running; only a paused run can switch models"}});
    let Some(out) = detail("r-paused", "paused", vec![profiles(), refused], vec![json!({"click": "[data-model]"}), json!({"click": "[data-confirm]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "r-paused is running; only a paused run can switch models"), "{out}");
    assert_eq!(count(end, "[data-model]"), 1, "the control is offered again: {out}");

    let broken = json!({"status": 500, "json": {"error": "the launch profiles could not be read"}});
    let Some(out) = detail("r-paused", "paused", vec![broken], vec![json!({"click": "[data-model]"})]) else { return };
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "the launch profiles could not be read"), "{out}");
    assert_eq!(count(end, "[data-confirm]"), 0, "nothing is asked: {out}");
    assert!(sent(&out, "POST", "/model").is_empty());

    let none = json!({"status": 200, "json": {"profiles": [], "implementer": null, "reviewer": null}});
    let Some(out) = detail("r-paused", "paused", vec![none], vec![json!({"click": "[data-model]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "No model profiles are defined."), "{out}");
    assert_eq!(count(last(&out), "[data-confirm]"), 0, "{out}");
}
