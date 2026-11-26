//! The Features page's archive controls follow the design system: "Archive completed (M)" ends work, so it is a
//! danger button with an archive icon and its question is answered by a danger button with Cancel beside it;
//! the "Archived (N)" toggle is a plain button that carries the archive icon.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = [
    "button.danger-button[data-archive-completed]",
    "[data-archive-completed] svg",
    ".confirm .confirm-question + .confirm-actions button.danger-button[data-confirm]",
    ".confirm .confirm-actions button[data-cancel]",
    "[data-archived-toggle] svg",
];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger(completed: usize) -> Value {
    let mut features: Vec<Value> = (0..completed).map(|i| json!({"name": format!("done-{i}"), "description": null, "status": "completed", "depends_on": [], "recorded_at": null})).collect();
    features.push(json!({"name": "next-one", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}));
    json!({"status": 200, "json": {"features": features, "remote_actions": true}})
}

fn archived() -> Value {
    let entry = |name: &str| json!({"name": name, "description": "an old one", "status": "completed", "depends_on": [], "recorded_at": null, "archived_at": "2026-11-20T10:15:00Z"});
    json!({"status": 200, "json": {"archived": [entry("old-alpha"), entry("old-beta")]}})
}

fn page(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let (archived, responses): (Vec<Value>, Vec<Value>) = responses.into_iter().partition(|r| r["json"]["archived"].is_array());
    run(json!({"url": "/projects/alpha/features", "responses": responses, "archived": archived, "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn archive_completed_is_a_danger_button_with_an_icon() {
    let Some(out) = page(vec![ledger(201), projects(), archived()], vec![]) else { return };
    let shown = last(&out);
    assert_eq!(count(shown, SELECTORS[0]), 1, "{out}");
    assert_eq!(count(shown, SELECTORS[1]), 1, "{out}");
}

#[test]
fn archive_completed_asks_with_a_danger_answer_beside_cancel() {
    let dry = json!({"status": 200, "json": {"archived": 101, "dry_run": true, "stripped": []}});
    let Some(out) = page(vec![ledger(201), projects(), archived(), dry], vec![json!({"click": "[data-archive-completed]"})]) else { return };
    let asked = last(&out);
    assert_eq!(count(asked, SELECTORS[2]), 1, "{out}");
    assert_eq!(count(asked, SELECTORS[3]), 1, "{out}");
}

#[test]
fn archived_toggle_has_an_icon() {
    let Some(out) = page(vec![ledger(2), projects(), archived()], vec![]) else { return };
    assert_eq!(count(last(&out), SELECTORS[4]), 1, "{out}");
}
