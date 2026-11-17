//! A gate whose tests passed only on a retry names them under its verify step on the Timeline, each with
//! the retry it passed on; a gate with none shows no such line.

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
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "verify", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let selectors = ["[data-step-flaky]", ".run-step [data-step-flaky]"];
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

fn verify_events(flaky: Option<Value>) -> Value {
    let mut list = vec![json!({"event_id": "e0", "kind": "stage.started", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"})];
    if let Some(flaky) = flaky {
        list.push(json!({"event_id": "e1", "kind": "gate.flaky_tests", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:35:07Z", "flaky_tests": flaky}));
    }
    list.push(json!({"event_id": "e2", "kind": "stage.completed", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:35:08Z"}));
    Value::Array(list)
}

#[test]
fn a_verify_step_names_each_test_that_passed_only_on_a_retry() {
    let flaky = json!([
        {"name": "orq-cli::pipeline two_runs_in_one_second", "retry": 1},
        {"name": "orq-core::store a_slow_lock", "retry": 2},
        {"name": "orq-core::store unknown_retry", "retry": null},
    ]);
    let Some(frame) = open_run(verify_events(Some(flaky))) else { return };
    let texts = texts(&frame);
    for line in [
        "Flaky: orq-cli::pipeline two_runs_in_one_second (passed on retry 1)",
        "Flaky: orq-core::store a_slow_lock (passed on retry 2)",
        "Flaky: orq-core::store unknown_retry (passed on a retry)",
    ] {
        assert!(texts.iter().any(|t| t == line), "{line}: {texts:?}");
    }
    assert_eq!(count(&frame, ".run-step [data-step-flaky]"), 3, "{frame}");
    assert_eq!(count(&frame, "[data-step-flaky]"), 3, "only the Timeline step lists them: {frame}");
}

#[test]
fn a_verify_step_with_no_flaky_test_has_no_flaky_line() {
    let Some(frame) = open_run(verify_events(None)) else { return };
    assert_eq!(count(&frame, "[data-step-flaky]"), 0, "{frame}");
    assert!(!texts(&frame).iter().any(|t| t.starts_with("Flaky")), "{:?}", texts(&frame));
}
