//! A run that failed because its feature was already marked 'completed' says so in plain words under the
//! failed step, and what resuming does about it; any other failure gets no such note.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": RUN, "responses": responses, "steps": steps, "selectors": ["[data-step-completed-note]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn stream() -> Value {
    json!({"status": 200, "stream": [], "end": "hold"})
}

fn failed_with(reason: &str) -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": "implement_and_review", "started_at": null, "updated_at": null});
    let events = json!([
        {"event_id": "e1", "kind": "stage.started", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"},
        {"event_id": "e2", "kind": "stage.failed", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z", "failure": {"reason": reason}},
    ]);
    json!({"status": 200, "json": {"run": header, "events": events, "next_cursor": null}})
}

fn shown(out: &Value) -> (Vec<String>, u64) {
    let frame = &out["frames"][1]["settled"];
    let texts = frame["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect();
    (texts, frame["matches"]["[data-step-completed-note]"].as_u64().unwrap())
}

#[test]
fn a_failure_because_the_feature_was_already_completed_is_explained_with_what_resume_does() {
    let reason = "init_feature refused: the feature is already marked 'completed'";
    let Some(out) = run(json!([failed_with(reason), projects(), stream()]), json!([{"submit": TOKEN}])) else { return };

    let (texts, notes) = shown(&out);
    assert_eq!(notes, 1, "{out}");
    assert!(texts.iter().any(|t| t.contains("already marked completed in the ledger") && t.contains("Resume puts it back")), "{texts:?}");
}

#[test]
fn any_other_failure_gets_no_such_note() {
    let Some(out) = run(json!([failed_with("the session exited"), projects(), stream()]), json!([{"submit": TOKEN}])) else { return };

    assert_eq!(shown(&out).1, 0, "{out}");
}
