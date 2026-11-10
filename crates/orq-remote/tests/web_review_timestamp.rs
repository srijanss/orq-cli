//! Each review row and the report show when the report was generated, as a plain readable time.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/reviews", "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn list(times: [Value; 3]) -> Value {
    let row = |id: &str, at: &Value| json!({"run_id": id, "feature": format!("feature-{id}"), "status": "ready", "stale": false, "generated_at": at});
    json!({"status": 200, "json": {"reviews": [row("a", &times[0]), row("b", &times[1]), row("c", &times[2])]}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn generated(frame: &Value) -> usize {
    texts(frame).iter().filter(|t| t.starts_with("Reviewed at")).count()
}

#[test]
fn a_row_shows_when_its_report_was_generated_and_a_missing_or_odd_time_shows_nothing() {
    let Some(out) = run(json!([list([json!("2026-11-03T10:20:30Z"), Value::Null, json!("<b>soon</b>")])]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(generated(frame), 1, "{out}");
    assert!(frame["settled"]["times"][0]["title"].as_str().unwrap().contains("2026"), "{out}");
    assert!(!texts(frame).iter().any(|t| t.contains("soon") || t.contains("T10:20")), "{out}");
}

#[test]
fn the_report_shows_when_it_was_generated() {
    let detail = json!({"status": 200, "json": {"stale": false, "link": null, "summary": ["x"], "risks": [], "files": [], "tests": [], "rounds": [], "generated_at": "2026-11-03T10:20:30Z"}});
    let Some(out) = run(json!([list([Value::Null, Value::Null, Value::Null]), detail]), json!([{"submit": TOKEN}, {"click": "button[data-open-run]", "index": 0}])) else { return };
    assert_eq!(generated(&out["frames"][2]), 1, "{out}");
    assert!(out["frames"][2]["settled"]["times"][0]["title"].as_str().unwrap().contains("2026"), "{out}");
}
