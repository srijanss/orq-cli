//! A session that changed files outside its features' files names them under its step on the Timeline;
//! a step with none shows no such line.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn open_run(events: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "implement_and_review", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let selectors = ["[data-step-undeclared]", ".run-step [data-step-undeclared]"];
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

fn implement_events(files: Option<Value>) -> Value {
    let mut list = vec![json!({"event_id": "e0", "kind": "stage.started", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"})];
    list.push(json!({"event_id": "e1", "kind": "stage.completed", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:07Z"}));
    if let Some(files) = files {
        list.push(json!({"event_id": "e2", "kind": "session.undeclared_files", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:08Z", "undeclared_files": files}));
    }
    Value::Array(list)
}

#[test]
fn an_implement_step_names_each_file_changed_outside_its_features_files() {
    let Some(frame) = open_run(implement_events(Some(json!(["src/other.rs", "fixed.txt", 7])))) else { return };
    let texts = texts(&frame);
    for line in ["Changed outside its features' files: src/other.rs", "Changed outside its features' files: fixed.txt"] {
        assert!(texts.iter().any(|t| t == line), "{line}: {texts:?}");
    }
    assert_eq!(count(&frame, ".run-step [data-step-undeclared]"), 2, "{frame}");
    assert_eq!(count(&frame, "[data-step-undeclared]"), 2, "only the Timeline step lists them: {frame}");
}

#[test]
fn a_step_with_no_undeclared_file_has_no_such_line() {
    let Some(frame) = open_run(implement_events(None)) else { return };
    assert_eq!(count(&frame, "[data-step-undeclared]"), 0, "{frame}");
    assert!(!texts(&frame).iter().any(|t| t.starts_with("Changed outside")), "{:?}", texts(&frame));
}
