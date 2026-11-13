//! A start the server warns about, such as the local target branch having commits its remote does not,
//! shows the warning beside the confirmation, by Start, Start with… or Start N marked.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const WARNING: &str = "main has 2 commits origin/main does not; the run starts from origin/main without them";

fn run(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": ["[data-marked-result]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger() -> Value {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("next-one"), feature("other")], "remote_actions": true}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn profiles() -> Value {
    json!({"status": 200, "json": {"profiles": ["opus"], "implementer": "opus", "reviewer": "opus"}})
}

fn warned(run: &str) -> Value {
    json!({"status": 202, "json": {"run": run, "warning": WARNING}})
}

fn texts(out: &Value) -> Vec<String> {
    let frame = out["frames"].as_array().unwrap().last().unwrap();
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn start_shows_the_servers_warning_with_the_started_run() {
    let steps = vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), warned("run-7")], steps) else { return };
    let expected = format!("Started run run-7. Warning: {WARNING}.");
    assert!(texts(&out).contains(&expected), "{out}");
}

#[test]
fn start_with_shows_the_servers_warning_with_the_started_run() {
    let steps = vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), profiles(), warned("run-8")], steps) else { return };
    let expected = format!("Started run run-8. Warning: {WARNING}.");
    assert!(texts(&out).contains(&expected), "{out}");
}

#[test]
fn start_marked_shows_each_starts_warning() {
    let steps = vec![json!({"click": "[data-mark=\"next-one\"]"}), json!({"click": "[data-mark=\"other\"]"}), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let plain = json!({"status": 202, "json": {"run": "run-10"}});
    let Some(out) = run("/projects/alpha/features", vec![ledger(), projects(), profiles(), warned("run-9"), plain], steps) else { return };
    let shown = texts(&out);
    assert!(shown.contains(&format!("Started run run-9 for next-one. Warning: {WARNING}.")), "{shown:?}");
    assert!(shown.contains(&"Started run run-10 for other.".to_string()), "no warning, none shown: {shown:?}");
}
