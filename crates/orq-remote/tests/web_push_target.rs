//! Pushing a project's target branch to origin, on the web: the "Push <target>" button beside the
//! "N commits on <target> not pushed" line of a GitHub project (its card, its project page and a
//! delivered merge_local run's page), GitLab's "deliver by MR review" in its place, why the push is
//! off, the confirmation that lists the commits with their runs, the POST of the confirmed `{sha}`,
//! and the line gone once the push is done.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const HEAD: &str = "2222222222222222222222222222222222222222";
const SELECTORS: [&str; 11] = [
    "[data-unpushed]",
    "[data-push-target]",
    "[data-push-target]:disabled",
    "[data-push-confirm]",
    "[data-push-commit]",
    "[data-push-commit-run]",
    "[data-push-send]",
    "[data-push-reason]",
    "[data-push-gitlab]",
    "[data-push-done]",
    "[data-run-pushed]",
];

fn harness(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node")
        .arg(format!("{dir}/tests/web_react_harness.mjs"))
        .arg(format!("{dir}/web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn as_token(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    harness(json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn as_session(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let auth = json!({"passkeys": true, "signed_in": true, "csrf": "c-1"});
    harness(json!({"url": url, "responses": responses, "auth": auth, "steps": steps, "selectors": SELECTORS}))
}

fn unpushed(provider: &str) -> Value {
    json!({"target": "main", "count": 2, "message": "2 commits on main not pushed", "subjects": ["a note by hand", "PUSH-01 work"], "provider": provider})
}

fn projects(unpushed: Value) -> Value {
    let card = json!({"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": null, "cleanup": [], "unpushed": unpushed});
    json!({"status": 200, "json": {"projects": [card]}})
}

/// What `GET /api/projects/alpha/push-target` answers: the push offered unless `reason`.
fn state(reason: Option<&str>, actions: bool) -> Value {
    let commits = json!([
        {"sha": HEAD, "short": "2222222", "subject": "a note by hand", "run": null},
        {"sha": "1111111111111111111111111111111111111111", "short": "1111111", "subject": "PUSH-01 work", "run": "run-1-PUSH-01"},
    ]);
    json!({"status": 200, "json": {"provider": "github", "remote": "origin", "target": "main", "sha": HEAD, "origin_sha": "0000000000000000000000000000000000000000", "count": 2, "commits": commits, "more": 0, "reason": reason, "remote_actions": actions}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn shows(frame: &Value, wanted: &str) -> bool {
    texts(frame).iter().any(|t| t.contains(wanted))
}

fn buttons(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn links(frame: &Value) -> Vec<(String, String)> {
    frame["settled"]["links"].as_array().unwrap().iter().map(|l| (l["href"].as_str().unwrap_or("").to_string(), l["text"].as_str().unwrap_or("").to_string())).collect()
}

fn frames(out: &Value) -> &Vec<Value> {
    out["frames"].as_array().unwrap()
}

fn last(out: &Value) -> &Value {
    frames(out).last().unwrap()
}

fn requests(out: &Value) -> Vec<(String, String, Value)> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["method"].as_str().unwrap_or("GET").to_string(), r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap_or("null")).unwrap_or(Value::Null)))
        .collect()
}

#[test]
fn a_github_project_card_offers_the_push_lists_the_commits_with_their_runs_and_the_line_goes_once_pushed() {
    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": ["run-1-PUSH-01"]}});
    let responses = vec![projects(unpushed("github")), state(None, true), pushed];
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"})];
    let Some(out) = as_session("/dashboard", responses, steps) else { return };

    let first = &frames(&out)[0];
    assert_eq!((count(first, "[data-unpushed]"), count(first, "[data-push-target]"), count(first, "[data-push-target]:disabled")), (1, 1, 0), "{out}");
    assert!(shows(first, "2 commits on main not pushed"), "{:?}", texts(first));
    assert!(buttons(first).iter().any(|b| b == "Push main"), "{:?}", buttons(first));
    assert_eq!(count(first, "[data-push-gitlab]"), 0, "{out}");

    let asking = &frames(&out)[1];
    assert_eq!((count(asking, "[data-push-confirm]"), count(asking, "[data-push-commit]"), count(asking, "[data-push-commit-run]")), (1, 2, 1), "{out}");
    for wanted in ["origin", "main", "origin/main..main", "2222222", "a note by hand", "1111111", "PUSH-01 work", "not from orq"] {
        assert!(shows(asking, wanted), "{wanted}: {:?}", texts(asking));
    }
    assert!(links(asking).contains(&("/projects/alpha/orchestrations/run-1-PUSH-01".to_string(), "run-1-PUSH-01".to_string())), "{:?}", links(asking));
    assert!(buttons(asking).iter().any(|b| b == "Push 2 commits"), "{:?}", buttons(asking));

    let sent = requests(&out);
    assert_eq!(sent[1], ("GET".to_string(), "/api/projects/alpha/push-target".to_string(), Value::Null), "{sent:?}");
    assert_eq!(sent[2], ("POST".to_string(), "/api/projects/alpha/push-target".to_string(), json!({"sha": HEAD})), "{sent:?}");
    assert_eq!(sent.len(), 3, "{sent:?}");

    let done = last(&out);
    assert_eq!((count(done, "[data-unpushed]"), count(done, "[data-push-confirm]"), count(done, "[data-push-done]")), (0, 0, 1), "{out}");
    assert!(shows(done, "Pushed 2 commits to origin/main."), "{:?}", texts(done));
}

#[test]
fn commits_merged_after_a_push_bring_the_line_and_its_push_back_on_the_next_refresh() {
    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": ["run-1-PUSH-01"]}});
    let newer = json!({"target": "main", "count": 1, "message": "1 commit on main not pushed", "subjects": ["PUSH-02 work"], "provider": "github"});
    let responses = vec![projects(unpushed("github")), state(None, true), pushed, projects(newer)];
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"}), json!({"tick": 30000})];
    let Some(out) = as_session("/dashboard", responses, steps) else { return };

    let done = &frames(&out)[2];
    assert_eq!(count(done, "[data-push-done]"), 1, "{out}");
    let refreshed = last(&out);
    assert_eq!((count(refreshed, "[data-unpushed]"), count(refreshed, "[data-push-done]"), count(refreshed, "[data-push-target]")), (1, 0, 1), "{out}");
    assert!(shows(refreshed, "1 commit on main not pushed"), "{:?}", texts(refreshed));
}

#[test]
fn a_gitlab_project_has_no_push_and_says_deliver_by_mr_review() {
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("gitlab"))], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-push-target]"), count(frame, "[data-push-gitlab]")), (0, 1), "{out}");
    assert!(shows(frame, "GitLab: deliver by MR review"), "{:?}", texts(frame));
    assert_eq!(requests(&out).len(), 1, "nothing is asked of the server: {out}");
}

#[test]
fn why_the_push_is_off_is_shown_and_nothing_is_sent() {
    // Signed in with the token: the button is off and says why, before anything is asked.
    let Some(out) = as_token("/dashboard", vec![projects(unpushed("github"))], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-push-target]"), count(frame, "[data-push-target]:disabled")), (1, 1), "{out}");
    assert!(shows(frame, "Pushing needs a passkey sign-in."), "{:?}", texts(frame));

    // The server's reason, such as a protected branch: shown, with no Push N commits.
    let reason = "main is protected on GitHub: deliver by review instead";
    let steps = vec![json!({"click": "[data-push-target]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(Some(reason), true)], steps.clone()) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-push-reason]"), count(frame, "[data-push-send]")), (1, 0), "{out}");
    assert!(shows(frame, reason), "{:?}", texts(frame));

    // Remote actions off: said so, with no Push N commits.
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, false)], steps) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-push-reason]"), count(frame, "[data-push-send]")), (1, 0), "{out}");
    assert!(shows(frame, "Remote actions are off on this server."), "{:?}", texts(frame));
    assert!(requests(&out).iter().all(|(method, _, _)| method == "GET"), "nothing was sent: {out}");
}

#[test]
fn the_project_page_offers_the_push_beside_its_not_pushed_line() {
    let queue = json!({"status": 200, "json": {"queue": {"entries": [], "count": 0, "paused": null, "unpushed": unpushed("github")}, "remote_actions": false}});
    let Some(out) = as_session("/projects/alpha/queue", vec![queue, projects(Value::Null)], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-unpushed]"), count(frame, "[data-push-target]")), (1, 1), "{out}");
}

fn run_page(unpushed: Value, events: Value) -> Value {
    let shown = json!({"run_id": "run-1-PUSH-01", "feature": "PUSH-01", "state": "completed", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main", "delivery": "merge_local", "unpushed": unpushed});
    json!({"status": 200, "json": {"run": shown, "events": events, "next_cursor": null, "remote_actions": true}})
}

#[test]
fn a_delivered_merge_local_run_page_offers_the_push_and_its_timeline_says_pushed() {
    let hold = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = as_session("/projects/alpha/orchestrations/run-1-PUSH-01", vec![run_page(unpushed("github"), json!([])), projects(Value::Null), hold.clone()], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-unpushed]"), count(frame, "[data-push-target]")), (1, 1), "{out}");

    let pushed = json!([{"event_id": "e-1", "kind": "target.pushed", "stage": null, "attempt": 1, "occurred_at": "2026-11-20T10:00:00Z", "pushed": {"remote": "origin", "target": "main"}}]);
    let Some(out) = as_session("/projects/alpha/orchestrations/run-1-PUSH-01", vec![run_page(Value::Null, pushed), projects(Value::Null), hold], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-unpushed]"), count(frame, "[data-run-pushed]")), (0, 1), "{out}");
    assert!(shows(frame, "Pushed to origin/main"), "{:?}", texts(frame));
}
