//! A request that has been superseded is cancelled, and its cancellation is not a failure.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "alpha", "status": "approved", "stale": false}]}})
}

#[test]
fn leaving_a_page_cancels_its_pending_request_and_keeps_the_token() {
    let held = json!({"hold": "fetch", "status": 200, "json": {"stale": false, "summary": [], "risks": [], "files": [], "tests": [], "rounds": []}});
    let Some(out) = run(json!([list(), held, list()]), json!([{"submit": "token"}, {"click": "button", "index": 0}, {"back": true}])) else { return };
    assert_eq!(out["requests"][1]["aborted"], true, "the request for the page that was left is cancelled: {out}");
    let last = &out["frames"][3]["settled"];
    assert!(last["inputs"].as_array().unwrap().is_empty(), "a cancelled request must not sign the user out: {out}");
    assert!(last["texts"].as_array().unwrap().iter().any(|t| t == "alpha"), "the list is shown again: {out}");
}

#[test]
fn a_request_still_wanted_is_not_cancelled() {
    let Some(out) = run(json!([list()]), json!([{"submit": "token"}])) else { return };
    assert_eq!(out["requests"][0]["aborted"], false, "{out}");
}

#[test]
fn every_request_carries_a_cancellation_signal() {
    let Some(out) = run(json!([list()]), json!([{"submit": "token"}])) else { return };
    assert!(out["requests"][0]["aborted"].is_boolean(), "{out}");
}
