//! Settings shows the running orq and the installed one, and says "A newer orq is installed" when they
//! differ. Signed in with a passkey and remote actions on, "Restart when idle" asks first, then sends POST
//! /api/server/restart; while a restart waits, Settings says "Restart pending: waiting for N runs" with a
//! Cancel that sends POST /api/server/restart/cancel. With the token alone it is not offered. A Start while
//! a restart is pending asks "orq will restart after this run; start anyway?".

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const SELECTORS: [&str; 3] = ["[data-restart-server]", "[data-restart-cancel]", "[data-confirm]"];
const QUESTION: &str = "Restart orq once no run is active? It restarts into the installed orq.";
const START_QUESTION: &str = "orq will restart after this run; start anyway?";
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

fn settings(on: bool, pending: bool, waiting: u64) -> Value {
    json!({"status": 200, "json": {
        "settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"},
        "server": {"running": "orq 0.1.0 (aaaaaaa 2026-01-01T00:00:00Z)", "installed": "orq 0.2.0 (bbbbbbb 2026-02-01T00:00:00Z)", "newer_installed": true, "restart_pending": pending, "waiting_for": waiting}
    }})
}

fn as_session(responses: Value, steps: Value) -> Option<Value> {
    run(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": {"/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]}},
        "responses": responses, "steps": steps, "selectors": SELECTORS
    }))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn says(frame: &Value, text: &str) -> bool {
    shown(frame).iter().any(|t| t.contains(text))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn settings_shows_both_builds_and_says_when_a_newer_one_is_installed() {
    let Some(out) = as_session(json!([settings(false, false, 0)]), json!([])) else { return };
    let frame = last(&out);
    assert!(says(frame, "orq 0.1.0 (aaaaaaa 2026-01-01T00:00:00Z)"), "{out}");
    assert!(says(frame, "orq 0.2.0 (bbbbbbb 2026-02-01T00:00:00Z)"), "{out}");
    assert!(says(frame, "A newer orq is installed"), "{out}");
    assert_eq!(count(frame, "[data-restart-server]"), 0, "not offered with remote actions off: {out}");
}

#[test]
fn settings_says_whether_systemd_restarts_the_server() {
    for text in ["Managed by systemd (restarts automatically)", "Not managed: stops on reboot"] {
        let mut reply = settings(false, false, 0);
        reply["json"]["server"]["supervision"] = json!(text);
        let Some(out) = as_session(json!([reply]), json!([])) else { return };
        assert!(says(last(&out), text), "{out}");
    }
}

#[test]
fn settings_shows_each_harness_the_servers_path_lacks() {
    let warning = "runs will fail: claude is not on the PATH the service gets (/usr/bin:/bin)";
    let mut reply = settings(false, false, 0);
    reply["json"]["server"]["harness_warnings"] = json!([warning]);
    let Some(out) = as_session(json!([reply]), json!([])) else { return };
    assert!(says(last(&out), warning), "{out}");
    let Some(out) = as_session(json!([settings(false, false, 0)]), json!([])) else { return };
    assert!(!says(last(&out), "runs will fail"), "no warning when the server sends none: {out}");
}

#[test]
fn restart_when_idle_asks_first_then_asks_the_server_and_shows_it_pending() {
    let responses = json!([settings(true, false, 0), {"status": 202, "json": {"restart_pending": true, "waiting_for": 1}}, settings(true, true, 1)]);
    let steps = json!([{"click": "[data-restart-server]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(responses, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], "[data-restart-server]"), 1, "{out}");
    assert!(says(&frames[1], QUESTION), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/server/restart", "{out}");
    assert_eq!(sent[0]["headers"]["X-CSRF-Token"], "c-1", "{out}");
    assert!(says(last(&out), "Restart pending: waiting for 1 run"), "{out}");
    assert_eq!(count(last(&out), "[data-restart-cancel]"), 1, "{out}");
}

#[test]
fn a_refused_restart_shows_the_servers_reason() {
    let refused = json!({"status": 409, "json": {"error": "orq runs from a build under target/"}});
    let steps = json!([{"click": "[data-restart-server]"}, {"click": "[data-confirm]"}]);
    let Some(out) = as_session(json!([settings(true, false, 0), refused]), steps) else { return };
    assert!(says(last(&out), "orq runs from a build under target/"), "{out}");
}

#[test]
fn cancel_drops_a_pending_restart() {
    let responses = json!([settings(false, true, 2), {"status": 200, "json": {"cancelled": true}}, settings(false, false, 0)]);
    let Some(out) = as_session(responses, json!([{"click": "[data-restart-cancel]"}])) else { return };
    assert!(says(&out["frames"][0], "Restart pending: waiting for 2 runs"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/server/restart/cancel", "{out}");
    assert_eq!(count(last(&out), "[data-restart-cancel]"), 0, "{out}");
}

#[test]
fn with_the_token_alone_restart_is_not_offered() {
    let Some(out) = run(json!({
        "url": "/settings", "token": "tok", "session": {"orq.token": "tok"}, "auth": {"passkeys": true, "signed_in": false},
        "responses": [settings(true, false, 0)], "steps": [], "selectors": SELECTORS
    })) else { return };
    assert_eq!(count(last(&out), "[data-restart-server]"), 0, "{out}");
    assert!(says(last(&out), "A newer orq is installed"), "{out}");
}

fn ledger(pending: bool) -> Value {
    let feature = json!({"name": "next-one", "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature], "remote_actions": true, "restart_pending": pending}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true}})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn start_question(pending: bool) -> Option<Vec<String>> {
    let steps = json!([{"submit": TOKEN}, {"click": "[data-start]"}]);
    let out = run(json!({"url": "/projects/alpha/features/next-one", "responses": [ledger(pending), runs(), projects()], "steps": steps, "selectors": ["[data-start]"]}))?;
    Some(shown(last(&out)))
}

#[test]
fn a_start_while_a_restart_is_pending_asks_whether_to_start_anyway() {
    let Some(texts) = start_question(true) else { return };
    assert!(texts.iter().any(|t| t == START_QUESTION), "{texts:?}");
    let Some(texts) = start_question(false) else { return };
    assert!(texts.iter().any(|t| t == "Start next-one?"), "{texts:?}");
    assert!(!texts.iter().any(|t| t == START_QUESTION), "{texts:?}");
}
