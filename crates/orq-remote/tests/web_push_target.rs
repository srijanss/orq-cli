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
const SELECTORS: [&str; 26] = [
    "details[data-unpushed-details]",
    "details[data-unpushed-details][open]",
    ".unpushed-warning",
    "[data-unpushed-details] [data-push-target]",
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
    "[data-push-sending]",
    "[data-push-send][aria-busy=\"true\"]",
    "[data-push-send]:disabled",
    "[data-push-modal]",
    "[data-modal-close]",
    "[data-push-modal] [data-push-confirm]",
    "[data-push-modal] [data-push-error]",
    "[data-modal-close]:disabled",
    "[data-push-modal] [data-cancel]",
    "[data-push-modal] [role=dialog]",
    "[data-push-checking]",
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
fn pressing_push_n_commits_shows_pushing_with_a_spinner_until_the_push_answers() {
    let pushed = json!({"status": 200, "hold": "fetch", "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let responses = vec![projects(unpushed("github")), state(None, true), pushed];
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"}), json!({"release": 2})];
    let Some(out) = as_session("/dashboard", responses, steps) else { return };

    let all = frames(&out);
    let sending = &all[all.len() - 2];
    assert_eq!((count(sending, "[data-push-sending]"), count(sending, "[data-push-send][aria-busy=\"true\"]"), count(sending, "[data-push-send]:disabled")), (1, 1, 1), "{out}");
    assert!(buttons(sending).iter().any(|b| b == "Pushing…"), "{:?}", buttons(sending));
    assert!(shows(sending, "Pushing 2 commits to origin/main…"), "{:?}", texts(sending));

    let done = last(&out);
    assert_eq!((count(done, "[data-push-sending]"), count(done, "[data-push-done]")), (0, 1), "{out}");
}

#[test]
fn a_failed_push_drops_the_spinner_and_offers_push_n_commits_again() {
    let rejected = json!({"status": 500, "hold": "fetch", "json": {"error": "rejected"}});
    let responses = vec![projects(unpushed("github")), state(None, true), rejected];
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"}), json!({"release": 2})];
    let Some(out) = as_session("/dashboard", responses, steps) else { return };

    let all = frames(&out);
    assert_eq!(count(&all[all.len() - 2], "[data-push-sending]"), 1, "{out}");
    let failed = last(&out);
    assert_eq!((count(failed, "[data-push-sending]"), count(failed, "[data-push-send][aria-busy=\"true\"]"), count(failed, "[data-push-send]"), count(failed, "[data-push-send]:disabled")), (0, 0, 1, 0), "{out}");
    assert!(buttons(failed).iter().any(|b| b == "Push 2 commits"), "{:?}", buttons(failed));
    assert!(shows(failed, "rejected"), "{:?}", texts(failed));
    assert!(!shows(failed, "Pushing 2 commits to origin/main…"), "{:?}", texts(failed));
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
fn new_commits_that_look_like_the_pushed_ones_or_follow_nothing_waiting_still_bring_the_push_back() {
    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": ["run-1-PUSH-01"]}});
    let at = |sha: &str| {
        let mut line = unpushed("github");
        line["sha"] = json!(sha);
        line
    };
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"}), json!({"tick": 30000})];

    // The same count and subjects at another tip are other commits.
    let responses = vec![projects(at(HEAD)), state(None, true), pushed.clone(), projects(at("3333333333333333333333333333333333333333"))];
    let Some(out) = as_session("/dashboard", responses, steps.clone()) else { return };
    assert_eq!(count(&frames(&out)[2], "[data-push-done]"), 1, "{out}");
    let refreshed = last(&out);
    assert_eq!((count(refreshed, "[data-unpushed]"), count(refreshed, "[data-push-done]"), count(refreshed, "[data-push-target]")), (1, 0, 1), "{out}");

    // Nothing waiting ends the pushed note: a line that comes back after it is shown with its Push.
    let mut steps = steps;
    steps.push(json!({"tick": 30000}));
    let responses = vec![projects(at(HEAD)), state(None, true), pushed, projects(Value::Null), projects(at(HEAD))];
    let Some(out) = as_session("/dashboard", responses, steps) else { return };
    let refreshed = last(&out);
    assert_eq!((count(refreshed, "[data-unpushed]"), count(refreshed, "[data-push-done]"), count(refreshed, "[data-push-target]")), (1, 0, 1), "{out}");
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

fn queue_page(line: Value) -> Value {
    json!({"status": 200, "json": {"queue": {"entries": [], "count": 0, "paused": null, "unpushed": line}, "remote_actions": true}})
}

#[test]
fn the_queue_pages_line_is_a_closed_details_with_the_push_outside_it_that_opens_to_the_subjects() {
    let steps = vec![json!({"click": "[data-unpushed-details] > summary"})];
    let Some(out) = as_session("/projects/alpha/queue", vec![queue_page(unpushed("github")), projects(Value::Null)], steps) else { return };
    let closed = &frames(&out)[0];
    assert_eq!((count(closed, "details[data-unpushed-details]"), count(closed, "details[data-unpushed-details][open]")), (1, 0), "{out}");
    assert_eq!((count(closed, "[data-push-target]"), count(closed, "[data-unpushed-details] [data-push-target]")), (1, 0), "the Push is beside the summary: {out}");
    assert!(shows(closed, "2 commits on main not pushed"), "{:?}", texts(closed));
    let opened = last(&out);
    assert_eq!(count(opened, "details[data-unpushed-details][open]"), 1, "{out}");
    for wanted in ["a note by hand", "PUSH-01 work"] {
        assert!(shows(opened, wanted), "{wanted}: {:?}", texts(opened));
    }
}

#[test]
fn the_push_works_with_the_line_closed() {
    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let responses = vec![queue_page(unpushed("github")), projects(Value::Null), state(None, true), pushed];
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"})];
    let Some(out) = as_session("/projects/alpha/queue", responses, steps) else { return };
    assert_eq!(count(&frames(&out)[1], "[data-push-confirm]"), 1, "{out}");
    assert!(requests(&out).iter().any(|(method, _, body)| method == "POST" && body == &json!({"sha": HEAD})), "{:?}", requests(&out));
    assert_eq!(count(last(&out), "[data-push-done]"), 1, "{out}");
}

#[test]
fn the_icon_and_message_carry_the_warning_class_on_the_card_the_queue_and_the_run_page_but_the_pushed_note_does_not() {
    let hold = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github"))], vec![]) else { return };
    assert_eq!((count(last(&out), ".unpushed-warning"), count(last(&out), "details[data-unpushed-details]")), (1, 0), "the card is one line with no details: {out}");

    let Some(out) = as_session("/projects/alpha/queue", vec![queue_page(unpushed("github")), projects(Value::Null)], vec![]) else { return };
    assert_eq!(count(last(&out), ".unpushed-warning"), 1, "{out}");

    let Some(out) = as_session("/projects/alpha/orchestrations/run-1-PUSH-01", vec![run_page(unpushed("github"), json!([])), projects(Value::Null), hold], vec![]) else { return };
    assert_eq!(count(last(&out), ".unpushed-warning"), 1, "{out}");

    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), pushed], steps) else { return };
    assert_eq!((count(last(&out), "[data-push-done]"), count(last(&out), ".unpushed-warning")), (1, 0), "{out}");
}

#[test]
fn the_spinner_does_not_spin_with_reduced_motion() {
    let css = std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let rule = |from: &str| from.split(".spinner {").nth(1).map(|r| r.split('}').next().unwrap().to_string());
    let spinning = rule(&css).expect("a .spinner rule");
    assert!(spinning.contains("animation:") && spinning.contains("spinner-turn"), "{spinning}");
    assert!(css.contains("@keyframes spinner-turn"), "the spinner's keyframes");
    let reduced = css.split("@media (prefers-reduced-motion: reduce) {").skip(1).find_map(|block| rule(block.split("\n}").next().unwrap())).expect("a .spinner rule in the reduced-motion block");
    assert_eq!(reduced.trim(), "animation: none;", "{reduced}");
}

#[test]
fn the_warning_token_is_defined_in_every_theme_block_and_the_line_uses_it() {
    let css = std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let themes = css.matches("--status-pending-bg:").count();
    assert!(themes >= 4, "four theme blocks: {themes}");
    assert_eq!(css.matches("--warning:").count(), themes, "one --warning per theme block");
    assert!(css.contains("--warning: #fcd34d") && css.contains("--warning: #92400e"), "dark and light values");
    let rule = css.split(".unpushed-warning").nth(1).expect("a .unpushed-warning rule");
    assert!(rule.split('}').next().unwrap().contains("var(--warning)"), "{rule}");
}

/// The confirmation is in the popup, and nowhere else on the page.
fn in_popup(frame: &Value) -> bool {
    count(frame, "[data-push-modal]") == 1 && count(frame, "[data-push-modal] [data-push-confirm]") == 1 && count(frame, "[data-push-confirm]") == 1
}

#[test]
fn pressing_push_opens_the_confirmation_in_a_popup() {
    let steps = vec![json!({"click": "[data-push-target]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true)], steps) else { return };
    assert_eq!(count(&frames(&out)[0], "[data-push-modal]"), 0, "{out}");
    let asking = last(&out);
    assert!(in_popup(asking), "{out}");
    assert_eq!(count(asking, "[data-modal-close]"), 1, "{out}");
    assert!(buttons(asking).iter().any(|b| b == "Push 2 commits"), "{:?}", buttons(asking));
}

#[test]
fn the_queue_pages_push_opens_the_popup_too() {
    let steps = vec![json!({"click": "[data-push-target]"})];
    let Some(out) = as_session("/projects/alpha/queue", vec![queue_page(unpushed("github")), projects(Value::Null), state(None, true)], steps) else { return };
    assert!(in_popup(last(&out)), "{out}");
}

#[test]
fn escape_close_cancel_and_the_backdrop_shut_the_popup_unsent() {
    for shut in [json!({"keydown": "Escape"}), json!({"click": "[data-modal-close]"}), json!({"click": "[data-push-modal] [data-cancel]"}), json!({"click": "[data-push-modal]"})] {
        let steps = vec![json!({"click": "[data-push-target]"}), shut.clone()];
        let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true)], steps) else { return };
        assert!(in_popup(&frames(&out)[1]), "{shut}: opened");
        let closed = last(&out);
        assert_eq!((count(closed, "[data-push-modal]"), count(closed, "[data-push-confirm]"), count(closed, "[data-push-target]")), (0, 0, 1), "{shut}: closed");
        assert!(requests(&out).iter().all(|(method, _, _)| method == "GET"), "{shut}: nothing was sent: {:?}", requests(&out));
    }
}

#[test]
fn escape_close_and_the_backdrop_do_nothing_while_the_push_is_sending() {
    let held = json!({"status": 200, "hold": "fetch", "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let steps = vec![
        json!({"click": "[data-push-target]"}),
        json!({"click": "[data-push-send]"}),
        json!({"keydown": "Escape"}),
        json!({"click": "[data-modal-close]"}),
        json!({"click": "[data-push-modal]"}),
    ];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), held], steps) else { return };
    let sending = last(&out);
    assert!(in_popup(sending), "{out}");
    assert_eq!((count(sending, "[data-push-sending]"), count(sending, "[data-modal-close]:disabled")), (1, 1), "{out}");
}

#[test]
fn a_push_from_the_popup_closes_it_and_shows_the_pushed_note() {
    let pushed = json!({"status": 200, "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), pushed], steps) else { return };
    assert!(requests(&out).iter().any(|(method, _, body)| method == "POST" && body == &json!({"sha": HEAD})), "{:?}", requests(&out));
    let done = last(&out);
    assert_eq!((count(done, "[data-push-modal]"), count(done, "[data-push-done]")), (0, 1), "{out}");
    assert!(shows(done, "Pushed 2 commits to origin/main."), "{:?}", texts(done));
}

#[test]
fn a_failed_push_keeps_the_popup_with_the_error() {
    let rejected = json!({"status": 500, "json": {"error": "rejected by the remote"}});
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), rejected], steps) else { return };
    let failed = last(&out);
    assert!(in_popup(failed), "{out}");
    assert_eq!(count(failed, "[data-push-modal] [data-push-error]"), 1, "{out}");
    assert!(shows(failed, "rejected by the remote"), "{:?}", texts(failed));
}

fn focused(frame: &Value, selector: &str) -> bool {
    frame["settled"]["focusMatches"][selector].as_bool().unwrap()
}

#[test]
fn the_popup_takes_the_focus_keeps_tab_inside_and_gives_it_back_to_push_on_close() {
    let steps = vec![
        json!({"click": "[data-push-target]"}),
        json!({"keydown": "Tab", "on": "[data-push-modal] [data-cancel]"}),
        json!({"keydown": "Tab", "shift": true, "on": "[data-modal-close]"}),
        json!({"keydown": "Escape"}),
    ];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true)], steps) else { return };
    let all = frames(&out);
    assert!(focused(&all[1], "[data-push-modal] [data-cancel]"), "opening focuses Cancel: {:?}", all[1]["settled"]["focusMatches"]);
    assert!(focused(&all[2], "[data-modal-close]"), "Tab from the last stop wraps to Close: {:?}", all[2]["settled"]["focusMatches"]);
    assert!(focused(&all[3], "[data-push-modal] [data-cancel]"), "Shift+Tab from Close wraps to Cancel: {:?}", all[3]["settled"]["focusMatches"]);
    assert_eq!(count(&all[4], "[data-push-modal]"), 0, "{out}");
    assert!(focused(&all[4], "[data-push-target]"), "closing gives the focus back to Push: {:?}", all[4]["settled"]["focusMatches"]);
}

#[test]
fn while_a_push_with_no_run_links_is_sending_tab_stays_on_the_popup() {
    let mut no_runs = state(None, true);
    no_runs["json"]["commits"][1]["run"] = Value::Null;
    let held = json!({"status": 200, "hold": "fetch", "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let steps = vec![
        json!({"click": "[data-push-target]"}),
        json!({"click": "[data-push-send]"}),
        json!({"keydown": "Tab"}),
        json!({"keydown": "Tab", "shift": true}),
    ];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), no_runs, held], steps) else { return };
    let all = frames(&out);
    assert_eq!((count(&all[2], "[data-push-sending]"), count(&all[2], "[data-push-commit-run]")), (1, 0), "{out}");
    assert!(focused(&all[2], "[data-push-modal] [role=dialog]"), "sending moves the focus to the popup: {:?}", all[2]["settled"]["focusMatches"]);
    assert!(focused(&all[3], "[data-push-modal] [role=dialog]"), "Tab stays on the popup: {:?}", all[3]["settled"]["focusMatches"]);
    assert!(focused(&all[4], "[data-push-modal] [role=dialog]"), "Shift+Tab stays on the popup: {:?}", all[4]["settled"]["focusMatches"]);
}

#[test]
fn shift_tab_on_the_popup_while_a_push_with_a_run_link_is_sending_goes_to_the_run_link() {
    let held = json!({"status": 200, "hold": "fetch", "json": {"from": "0000000000000000000000000000000000000000", "to": HEAD, "count": 2, "runs": []}});
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"click": "[data-push-send]"}), json!({"keydown": "Tab", "shift": true})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), held], steps) else { return };
    let all = frames(&out);
    assert_eq!((count(&all[2], "[data-push-sending]"), count(&all[2], "[data-push-commit-run]")), (1, 1), "{out}");
    assert!(focused(&all[2], "[data-push-modal] [role=dialog]"), "sending moves the focus to the popup: {:?}", all[2]["settled"]["focusMatches"]);
    assert!(focused(&all[3], "[data-push-commit-run]"), "Shift+Tab from the popup goes to its last stop: {:?}", all[3]["settled"]["focusMatches"]);
}

#[test]
fn after_a_failed_push_tab_and_shift_tab_on_the_popup_go_to_its_first_and_last_stops() {
    let rejected = json!({"status": 500, "hold": "fetch", "json": {"error": "rejected"}});
    let steps = vec![
        json!({"click": "[data-push-target]"}),
        json!({"click": "[data-push-send]"}),
        json!({"release": 2}),
        json!({"keydown": "Tab", "shift": true}),
        json!({"focus": "[data-push-modal] [role=dialog]"}),
        json!({"keydown": "Tab"}),
    ];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true), rejected], steps) else { return };
    let all = frames(&out);
    assert_eq!((count(&all[3], "[data-push-sending]"), count(&all[3], "[data-push-send]:disabled"), count(&all[3], "[data-push-modal] [data-push-error]")), (0, 0, 1), "{out}");
    assert!(focused(&all[3], "[data-push-modal] [role=dialog]"), "the focus stays on the popup after the failure: {:?}", all[3]["settled"]["focusMatches"]);
    assert!(focused(&all[4], "[data-push-modal] [data-cancel]"), "Shift+Tab from the popup goes to Cancel: {:?}", all[4]["settled"]["focusMatches"]);
    assert!(focused(&all[6], "[data-modal-close]"), "Tab from the popup goes to Close: {:?}", all[6]["settled"]["focusMatches"]);
}

#[test]
fn only_the_commit_list_scrolls_inside_the_popup() {
    let css = std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let rule = css.split(".push-confirm .push-commits {").nth(1).expect("a .push-confirm .push-commits rule");
    let rule = rule.split('}').next().unwrap();
    assert!(rule.contains("max-height:") && rule.contains("overflow-y: auto"), "{rule}");
}

#[test]
fn a_commits_run_link_closes_the_popup_and_goes_to_the_run() {
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"nav": "[data-push-commit-run]"})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), state(None, true)], steps) else { return };
    let gone = last(&out);
    assert_eq!(gone["settled"]["path"], "/projects/alpha/orchestrations/run-1-PUSH-01", "{out}");
    assert_eq!(count(gone, "[data-push-modal]"), 0, "{out}");
}

/// The push-target GET held until a `{"release": 1}` step.
fn held_state() -> Value {
    let mut held = state(None, true);
    held["hold"] = json!("fetch");
    held
}

#[test]
fn pressing_push_main_shows_a_spinner_in_the_button_while_it_is_checked() {
    let in_button = "[data-push-target] [data-push-checking]";
    let visible = ".push-target > [role=status]:not(.sr-only)";
    let mut selectors = SELECTORS.to_vec();
    selectors.extend([in_button, visible]);
    let auth = json!({"passkeys": true, "signed_in": true, "csrf": "c-1"});
    let steps = json!([{"click": "[data-push-target]"}]);
    let Some(out) = harness(json!({"url": "/dashboard", "responses": [projects(unpushed("github")), held_state()], "auth": auth, "steps": steps, "selectors": selectors})) else { return };

    assert_eq!(count(&frames(&out)[0], "[data-push-checking]"), 0, "{out}");
    let checking = last(&out);
    assert_eq!((count(checking, in_button), count(checking, "[data-push-checking]"), count(checking, "[data-push-target]:disabled")), (1, 1, 1), "{out}");
    assert!(buttons(checking).iter().any(|b| b == "Push main"), "{:?}", buttons(checking));
    assert_eq!(count(checking, visible), 0, "{out}");
    assert!(shows(checking, "Checking…"), "{:?}", texts(checking));
    assert_eq!(count(checking, "[data-push-modal]"), 0, "{out}");
}

#[test]
fn the_checking_spinner_clears_when_the_check_answers() {
    let steps = vec![json!({"click": "[data-push-target]"}), json!({"release": 1})];
    let Some(out) = as_session("/dashboard", vec![projects(unpushed("github")), held_state()], steps) else { return };

    let all = frames(&out);
    assert_eq!(count(&all[all.len() - 2], "[data-push-checking]"), 1, "{out}");
    let asking = last(&out);
    assert_eq!((count(asking, "[data-push-checking]"), count(asking, "[data-push-modal] [data-push-confirm]")), (0, 1), "{out}");
    assert!(!shows(asking, "Checking…"), "{:?}", texts(asking));
}
