//! A features list opens on the work still to do: in-progress features first, then pending, then drafts,
//! and only then the finished ones (completed, abandoned, anything else). Within a state the ledger's own
//! order holds.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

#[test]
fn unfinished_features_come_before_finished_ones() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let ledger = json!({"status": 200, "json": {"features": [
        feature("done-a", "completed"),
        feature("sketch-a", "draft"),
        feature("gone-a", "abandoned"),
        feature("next-a", "pending"),
        feature("busy-a", "in_progress"),
        feature("done-b", "completed"),
        feature("sketch-b", "draft"),
        feature("busy-b", "in_progress"),
        feature("next-b", "pending")
    ]}});
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 9, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger, projects], "steps": [{"submit": TOKEN}]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap();
    let names = ["busy-a", "busy-b", "next-a", "next-b", "sketch-a", "sketch-b", "done-a", "done-b", "gone-a"];
    let texts: Vec<&str> = last["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).filter(|t| names.contains(t)).collect();
    assert_eq!(texts, names, "{out}");
}
