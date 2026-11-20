//! Files whose formatting orq undid after a session, outside its features' files, are listed under
//! the step on the Timeline with the files the session changed outside them, each marked as undone.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn open_run(events: Value, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "implement_and_review", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let all: Vec<Value> = std::iter::once(json!({"submit": TOKEN})).chain(steps).collect();
    let selectors = ["[data-step-undeclared]", "[data-step-format-reverted]", ".run-step [data-step-format-reverted]"];
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": [page, projects()], "steps": all, "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"].as_array().unwrap().last().unwrap().clone())
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

fn implement_events(undeclared: Option<Value>, reverted: Value) -> Value {
    let mut list = vec![json!({"event_id": "e0", "kind": "stage.started", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"})];
    list.push(json!({"event_id": "e1", "kind": "stage.completed", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:07Z"}));
    list.push(json!({"event_id": "e2", "kind": "session.format_reverted", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:08Z", "format_reverted": reverted}));
    if let Some(files) = undeclared {
        list.push(json!({"event_id": "e3", "kind": "session.undeclared_files", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:09Z", "undeclared_files": files}));
    }
    Value::Array(list)
}

#[test]
fn an_implement_step_lists_the_files_whose_formatting_was_undone_with_the_undeclared_ones() {
    let opened = vec![json!({"click": "[data-step-undeclared-toggle]"})];
    let events = implement_events(Some(json!(["src/real.rs"])), json!(["src/fmt.rs", 7]));
    let Some(frame) = open_run(events, opened) else { return };
    let texts = texts(&frame);
    for line in ["Changed outside its features' files: 2 files", "src/real.rs", "src/fmt.rs (formatting undone)"] {
        assert!(texts.iter().any(|t| t == line), "{line}: {texts:?}");
    }
    assert_eq!(count(&frame, "[data-step-undeclared]"), 1, "{frame}");
    assert_eq!(count(&frame, ".run-step [data-step-format-reverted]"), 1, "{frame}");
    assert_eq!(count(&frame, "[data-step-format-reverted]"), 1, "only the Timeline step lists them: {frame}");
}
