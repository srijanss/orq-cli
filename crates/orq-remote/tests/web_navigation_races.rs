//! A slow answer for a page the person has already left must not change the
//! page they are on, nor take their token away.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": responses, "steps": steps});
    if !web_deps::ready() {
        return None;
    }
    match Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output() {
        Ok(output) => {
            assert!(output.status.success(), "the harness failed: {}", String::from_utf8_lossy(&output.stderr));
            Some(serde_json::from_slice(&output.stdout).expect("the harness prints JSON"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: node is not installed");
            None
        }
        Err(error) => panic!("cannot run node: {error}"),
    }
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "first-feature", "status": "approved", "stale": false}]}})
}

fn report() -> Value {
    json!({"run_id": "run-1", "stale": false, "link": null, "summary": ["OLD-DETAIL"], "risks": [], "files": [], "tests": [], "rounds": []})
}

fn texts(out: &Value, frame: usize) -> String {
    out["frames"][frame]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

/// Opens the run, goes back to the list while the detail is still pending, then
/// lets the obsolete detail request finish in the given way.
fn leave_then_late_answer(late: Value) -> Option<Value> {
    run(
        json!([list(), late, list(), list()]),
        json!([{"submit": TOKEN}, {"click": "button[data-open-run]", "index": 0}, {"back": true}, {"release": 1}, {"click": "button[data-open-run]", "index": 0}]),
    )
}

#[test]
fn a_late_answer_for_a_page_that_was_left_changes_nothing_and_keeps_the_token() {
    let mut late_answers = vec![
        json!({"hold": "fetch", "status": 200, "json": report()}),
        json!({"hold": "fetch", "status": 401}),
        json!({"hold": "fetch", "reject": true}),
        json!({"hold": "json", "status": 200, "json": report()}),
    ];
    late_answers.push(json!({"hold": "fetch", "status": 503}));
    for late in late_answers {
        let Some(out) = leave_then_late_answer(late.clone()) else { return };
        // After the late answer is released, the list is still what is shown.
        let shown = texts(&out, 4);
        assert!(shown.contains("first-feature"), "{late}: {shown}");
        assert!(!shown.contains("OLD-DETAIL") && !shown.contains("could not be read") && !shown.contains("Could not sign in"), "{late}: {shown}");
        assert!(out["frames"][4]["settled"]["inputs"].as_array().unwrap().is_empty(), "{late}: the token field came back");
        // The token is still held: the next request carries it.
        let requests = out["requests"].as_array().unwrap();
        let last = requests.last().unwrap();
        assert_eq!(last["headers"]["Authorization"], format!("Bearer {TOKEN}"), "{late}: {out}");
    }
}
