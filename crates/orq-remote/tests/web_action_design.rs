//! The page's actions follow the design system: what moves a run or feature forward (Start, Promote, Resume,
//! Accept) is a primary button, what ends or throws work away (Stop, Discard) is a danger button, and each
//! carries its icon. A question is its own confirm block: the sentence above, then the answer in the same tone
//! as the action and Cancel beside it.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 1, "reviews_waiting": 0, "failed_runs": 1, "failed_window_hours": 24}]}})
}

fn runs() -> Value {
    let one = |id: &str, state: &str| json!({"run_id": id, "feature": id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    json!({"status": 200, "json": {"runs": [one("r-run", "running"), one("r-fail", "failed")], "next_cursor": null, "total": 2, "running": 1, "remote_actions": true}})
}

fn ledger() -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("sketch", "draft"), feature("next-one", "pending")], "remote_actions": true}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

const RUN_SELECTORS: [&str; 12] = [
    "button.primary-button[data-resume]",
    "button.primary-button[data-accept]",
    "button.danger-button[data-discard]",
    "button.danger-button[data-stop]",
    "button[data-pause]",
    "[data-resume] svg",
    "[data-accept] svg",
    "[data-discard] svg",
    "[data-stop] svg",
    "[data-pause] svg",
    ".confirm .confirm-question + .confirm-actions button.danger-button[data-confirm]",
    ".confirm .confirm-actions button[data-cancel]",
];

#[test]
fn run_actions_are_primary_or_danger_buttons_with_icons() {
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations", "responses": [runs(), projects()], "steps": [{"submit": TOKEN}], "selectors": RUN_SELECTORS})) else { return };
    let shown = last(&out);
    for selector in &RUN_SELECTORS[..10] {
        assert_eq!(count(shown, selector), 1, "{selector}: {out}");
    }
}

#[test]
fn a_danger_action_asks_in_a_confirm_block_answered_by_a_danger_button() {
    let stop = vec![json!({"submit": TOKEN}), json!({"click": "[data-stop]"})];
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations", "responses": [runs(), projects()], "steps": stop, "selectors": RUN_SELECTORS})) else { return };
    let asked = last(&out);
    assert_eq!(count(asked, RUN_SELECTORS[10]), 1, "{out}");
    assert_eq!(count(asked, RUN_SELECTORS[11]), 1, "{out}");
}

#[test]
fn starting_or_promoting_a_feature_is_a_primary_button_and_its_question_a_primary_confirm() {
    let selectors = ["button.primary-button[data-start]", "[data-start] svg", "button.primary-button[data-promote]", "[data-promote] svg", ".confirm .confirm-question + .confirm-actions button.primary-button[data-confirm]"];
    let start = vec![json!({"submit": TOKEN}), json!({"click": "[data-start]"})];
    let Some(out) = run(json!({"url": "/projects/alpha/features/next-one", "responses": [ledger(), runs(), projects()], "steps": start, "selectors": selectors})) else { return };
    let shown = &out["frames"][1];
    assert_eq!(count(shown, selectors[0]), 1, "{out}");
    assert_eq!(count(shown, selectors[1]), 1, "{out}");
    assert_eq!(count(last(&out), selectors[4]), 1, "{out}");
    let Some(out) = run(json!({"url": "/projects/alpha/features/sketch", "responses": [ledger(), runs(), projects()], "steps": [{"submit": TOKEN}], "selectors": selectors})) else { return };
    assert_eq!(count(last(&out), selectors[2]), 1, "{out}");
    assert_eq!(count(last(&out), selectors[3]), 1, "{out}");
}
