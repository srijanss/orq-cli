//! A design group member's gate says its scope in its verify step on the Timeline: the apps whose tests it
//! ran and the member it ran after. The full gate over the whole branch says nothing more.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SCOPE: &str = "[data-step-gate-scope]";

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// The run page after signing in, with one verify step whose start says `scope`: its settled frame.
fn open_run(scope: Option<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut start = json!({"event_id": "e0", "kind": "stage.started", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"});
    if let Some(scope) = scope {
        start["gate_scope"] = scope;
    }
    let events = json!([start, {"event_id": "e1", "kind": "stage.completed", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:35:08Z"}]);
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "verify", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let selectors = [SCOPE, ".run-step [data-step-gate-scope]"];
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": [page, projects()], "steps": [{"submit": TOKEN}], "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"][1].clone())
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

#[test]
fn a_member_gates_step_names_the_apps_it_tested_and_the_member_it_ran_after() {
    for (scope, line) in [
        (json!({"scope": "member", "member": "std", "apps": ["landingpage"]}), "Member gate: landingpage tests (after std)"),
        (json!({"scope": "member", "member": "fa", "apps": ["blog", "shop"]}), "Member gate: blog, shop tests (after fa)"),
        (json!({"scope": "member", "member": "fa", "apps": []}), "Member gate: no app tests (after fa)"),
        (json!({"scope": "member", "member": "std"}), "Member gate (after std)"),
    ] {
        let Some(frame) = open_run(Some(scope)) else { return };
        assert_eq!(count(&frame, ".run-step [data-step-gate-scope]"), 1, "{line}: {frame}");
        assert!(texts(&frame).iter().any(|t| t == line), "{line}: {:?}", texts(&frame));
    }
}

#[test]
fn the_full_gates_step_and_a_gate_with_no_scope_say_nothing_more() {
    for scope in [Some(json!({"scope": "full"})), None] {
        let Some(frame) = open_run(scope) else { return };
        assert_eq!(count(&frame, SCOPE), 0, "{frame}");
        assert!(!texts(&frame).iter().any(|t| t.starts_with("Member gate")), "{:?}", texts(&frame));
    }
}
