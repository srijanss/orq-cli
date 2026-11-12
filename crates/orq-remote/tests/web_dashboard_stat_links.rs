//! The Dashboard's counts open what they count: Active runs opens the Orchestrations page and Reviews
//! waiting the Reviews page; on a project card, Features, Active runs and Reviews waiting open that
//! project's own Features, Orchestrations and Reviews.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 3, "active_runs": 1, "reviews_waiting": 1, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn opened(click: &str) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let filler = json!({"status": 200, "json": {}});
    let scenario = json!({"url": "/dashboard", "responses": [projects(), filler, filler, filler, filler], "steps": [{"submit": TOKEN}, {"click": click}]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"][2]["settled"]["path"].clone())
}

#[test]
fn the_dashboard_totals_open_the_orchestrations_and_reviews_pages() {
    for (click, path) in [("[data-stat-link=\"runs\"]", "/orchestrations"), ("[data-stat-link=\"reviews\"]", "/reviews")] {
        let Some(at) = opened(click) else { return };
        assert_eq!(at, json!(path), "{click}");
    }
}

#[test]
fn a_project_cards_counts_open_that_projects_pages() {
    for (part, path) in [("features", "/projects/beta/features"), ("runs", "/projects/beta/orchestrations"), ("reviews", "/projects/beta/reviews")] {
        let Some(at) = opened(&format!("[data-card-stat=\"beta/{part}\"]")) else { return };
        assert_eq!(at, json!(path), "{part}");
    }
}
