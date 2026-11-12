//! On a wide screen Add feature opens its form in a popup over the list: a dialog with a title and a close
//! button, which Escape or Close shuts without adding anything. Its question stays in the popup. On a phone the
//! form opens in the page, under the toolbar, as before.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = [
    ".feature-modal > .feature-modal-box[role=dialog][aria-modal=true] .feature-editor-fields [data-field-name]",
    ".feature-modal-box > .feature-modal-head > button[data-modal-close]",
    ".feature-modal",
    ".feature-modal-box .confirm [data-confirm]",
    "[data-feature-add]",
];

fn ledger() -> Value {
    json!({"status": 200, "json": {"features": [{"name": "f1", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": true}})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run(steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger(), projects()], "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn add_feature_opens_a_dialog_with_its_question_inside_and_close_shuts_it() {
    let steps = vec![
        json!({"click": "[data-feature-add]"}),
        json!({"set": "[data-field-name]", "value": "new-one"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-cancel]"}),
        json!({"click": "[data-modal-close]"}),
    ];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[0]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "{out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 1, "the question stays in the popup: {out}");
    let last = frames.last().unwrap();
    assert_eq!(count(last, SELECTORS[2]), 0, "closed: {out}");
    assert_eq!(count(last, SELECTORS[4]), 1, "{out}");
    assert!(out["requests"].as_array().unwrap().iter().all(|r| r["method"] != "POST"), "nothing added: {out}");
}

#[test]
fn the_popup_is_over_the_page_on_a_wide_screen_and_in_the_page_on_a_phone() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let modal = css.lines().find(|line| line.starts_with(".feature-modal {")).expect("a .feature-modal rule");
    assert!(modal.contains("position: fixed") && modal.contains("inset: 0"), "{modal}");
    let phone = css.split("@media (max-width: 700px) {").skip(1).map(|block| &block[..block.find("\n}").unwrap()]).find(|block| block.contains(".feature-modal")).expect("a phone rule for the popup");
    assert!(phone.contains(".feature-modal, .feature-modal-box { display: contents; }"), "{phone}");
    assert!(phone.contains(".feature-modal-head { display: none; }"), "{phone}");
    // The popup drops the fields' own frame, but only where it is a popup; on a phone the fields keep it.
    assert!(css.contains("@media (min-width: 701px) { .feature-modal-box .feature-editor-fields { max-width: none; padding: 0; border: 0; } }"), "frameless only in the popup");
    assert!(!css.lines().any(|line| line.starts_with(".feature-modal-box .feature-editor-fields")), "not on a phone");
}
