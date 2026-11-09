//! Start, Pause and Resume in the page: shown only when the server says remote actions are on and the
//! feature or run allows them, confirmed by name first, and answered with the result or the reason.
//! Nothing else (merge, approve, restart, edit) is ever offered.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

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
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

fn ledger(actions: Option<bool>) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    let mut body = json!({"features": [feature("sketch", "draft"), feature("next-one", "pending"), feature("busy", "in_progress"), feature("done-one", "completed")]});
    if let Some(on) = actions {
        body["remote_actions"] = json!(on);
    }
    json!({"status": 200, "json": body})
}

fn runs(actions: Option<bool>) -> Value {
    let one = |id: &str, feature: &str, state: &str| json!({"run_id": id, "feature": feature, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    let mut body = json!({"runs": [one("r-run", "going", "running"), one("r-paused", "waiting", "paused"), one("r-done", "finished", "completed"), one("r-fail", "broken", "failed"), one("r-review", "reviewing", "needs_review"), one("r-ask", "asked", "pause_requested")], "next_cursor": null, "total": 6, "running": 1});
    if let Some(on) = actions {
        body["remote_actions"] = json!(on);
    }
    json!({"status": 200, "json": body})
}

fn frame_texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

const SELECTORS: [&str; 5] = ["[data-start]", "[data-pause]", "[data-resume]", "[data-confirm]", "[data-cancel]"];

fn features_page(actions: Option<bool>, extra_responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), projects()];
    responses.extend(extra_responses);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/features", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn runs_page(actions: Option<bool>, extra_responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![runs(actions), projects()];
    responses.extend(extra_responses);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn nothing_can_be_started_paused_or_resumed_unless_the_server_says_actions_are_on() {
    for actions in [None, Some(false)] {
        let Some(out) = features_page(actions, vec![], vec![]) else { return };
        for selector in SELECTORS {
            assert_eq!(count(&out["frames"][1], selector), 0, "features {actions:?} {selector}");
        }
        let Some(out) = runs_page(actions, vec![], vec![]) else { return };
        for selector in SELECTORS {
            assert_eq!(count(&out["frames"][1], selector), 0, "runs {actions:?} {selector}");
        }
    }
}

#[test]
fn with_actions_on_only_a_pending_feature_can_be_started_and_only_running_or_paused_runs_get_a_control() {
    let Some(out) = features_page(Some(true), vec![], vec![]) else { return };
    assert_eq!(count(&out["frames"][1], "[data-start]"), 1, "{out}");
    let Some(out) = runs_page(Some(true), vec![], vec![]) else { return };
    assert_eq!(count(&out["frames"][1], "[data-pause]"), 1, "{out}");
    assert_eq!(count(&out["frames"][1], "[data-resume]"), 1, "{out}");
    // Nothing is sent just by showing them.
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").count(), 0);
}

#[test]
fn a_start_asks_for_the_feature_by_name_first_and_cancelling_sends_nothing() {
    let Some(out) = features_page(Some(true), vec![], vec![json!({"click": "[data-start]"}), json!({"click": "[data-cancel]"})]) else { return };
    let asked = frame_texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t.contains("Start next-one?")), "{asked:?}");
    assert_eq!(count(&out["frames"][2], "[data-confirm]"), 1);
    assert_eq!(count(&out["frames"][3], "[data-confirm]"), 0, "cancelled");
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").count(), 0, "{out}");
}

#[test]
fn a_confirmed_start_posts_with_the_token_and_shows_the_run_it_started() {
    let reply = json!({"status": 202, "json": {"run": "run-9"}});
    let Some(out) = features_page(Some(true), vec![reply], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    let post = out["requests"].as_array().unwrap().iter().find(|r| r["method"] == "POST").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(post["url"], "/api/projects/alpha/features/next-one/runs");
    assert_eq!(post["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(frame_texts(last).iter().any(|t| t.contains("run-9")), "{last}");
}

#[test]
fn a_refusal_is_shown_as_the_servers_reason_and_a_failure_as_a_generic_line() {
    let refused = json!({"status": 409, "json": {"error": "next-one has a plan problem"}});
    let Some(out) = features_page(Some(true), vec![refused], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(frame_texts(last).iter().any(|t| t.contains("next-one has a plan problem")), "{last}");
    let failed = json!({"status": 500, "json": {"error": "the run could not be started"}});
    let Some(out) = features_page(Some(true), vec![failed], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(frame_texts(last).iter().any(|t| t.contains("could not be started")), "{last}");
}

#[test]
fn pause_and_resume_post_to_their_own_routes_after_a_confirmation_naming_the_run() {
    let ok = json!({"status": 202, "json": {"run": "r-run"}});
    let Some(out) = runs_page(Some(true), vec![ok.clone()], vec![json!({"click": "[data-pause]"})]) else { return };
    assert!(frame_texts(&out["frames"][2]).iter().any(|t| t.contains("Pause r-run?")), "{out}");
    let Some(out) = runs_page(Some(true), vec![ok.clone()], vec![json!({"click": "[data-pause]"}), json!({"click": "[data-confirm]"})]) else { return };
    let post = out["requests"].as_array().unwrap().iter().find(|r| r["method"] == "POST").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(post["url"], "/api/projects/alpha/runs/r-run/pause");
    let Some(out) = runs_page(Some(true), vec![ok], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    let post = out["requests"].as_array().unwrap().iter().find(|r| r["method"] == "POST").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(post["url"], "/api/projects/alpha/runs/r-paused/resume");
}

#[test]
fn each_row_asks_for_itself_and_a_confirmed_action_posts_once() {
    let Some(out) = runs_page(Some(true), vec![], vec![json!({"click": "[data-pause]"}), json!({"click": "[data-resume]"})]) else { return };
    assert_eq!(count(&out["frames"][2], "[data-confirm]"), 1, "{out}");
    assert_eq!(count(&out["frames"][3], "[data-confirm]"), 2, "each row keeps its own question");
    let ok = json!({"status": 202, "json": {"run": "run-9"}});
    let Some(out) = features_page(Some(true), vec![ok], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").count(), 1, "{out}");
}

#[test]
fn a_passkey_session_posts_with_its_csrf_token_and_never_a_bearer_token() {
    if !web_deps::ready() {
        return;
    }
    let reply = json!({"status": 202, "json": {"run": "run-9"}});
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger(Some(true)), projects(), reply], "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"}, "steps": [{"click": "[data-start]"}, {"click": "[data-confirm]"}]});
    let Some(out) = run(scenario) else { return };
    let post = out["requests"].as_array().unwrap().iter().find(|r| r["method"] == "POST").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(post["headers"]["X-CSRF-Token"], "c-1", "{post}");
    assert!(post["headers"].get("Authorization").is_none(), "{post}");
}

#[test]
fn a_successful_action_refreshes_the_list_and_does_not_offer_itself_again() {
    let ok = json!({"status": 202, "json": {"run": "run-9"}});
    let Some(out) = features_page(Some(true), vec![ok, ledger(Some(true))], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(count(last, "[data-start]"), 0, "the same start is not offered again: {last}");
    assert_eq!(count(last, "[data-confirm]"), 0);
    assert!(frame_texts(last).iter().any(|t| t.contains("run-9")), "{last}");
    let gets = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "GET" && r["url"] == "/api/projects/alpha/features").count();
    assert_eq!(gets, 2, "the ledger is read again after the start: {out}");

    let Some(out) = runs_page(Some(true), vec![json!({"status": 202, "json": {"run": "r-paused"}}), runs(Some(true))], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(count(last, "[data-resume]"), 0, "{last}");
    assert_eq!(count(last, "[data-pause]"), 1, "other rows keep their controls");

    // A refusal leaves the control usable.
    let refused = json!({"status": 409, "json": {"error": "no"}});
    let Some(out) = features_page(Some(true), vec![refused], vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(count(out["frames"].as_array().unwrap().last().unwrap(), "[data-start]"), 1);
}
