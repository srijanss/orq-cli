//! The feature editor's Description is a textarea that keeps line breaks, with a Write/Preview switch that
//! shows the text as Markdown. What is saved is exactly what was typed.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 6] = ["textarea[data-field-description]", "input[data-field-description]", "[data-description-preview]", "[data-description-write]", "[data-description-rendered]", "[data-description-rendered] strong"];
const TYPED: &str = "First line\n\n**bold** second\n- item";

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn edit(steps: Vec<Value>) -> Option<Value> {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    let ledger = json!({"status": 200, "json": {"features": [feature("next-one", "pending")], "remote_actions": true}});
    let runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}});
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let fields = json!({"status": 200, "json": {"fields": {"name": "next-one", "description": "One thing", "depends_on": [], "parallelizable": null, "test_file": null, "target_files": []}, "version": "v1"}});
    let saved = json!({"status": 200, "json": {"feature": "next-one"}});
    let mut all = vec![json!({"submit": TOKEN}), json!({"click": "[data-feature-edit]"})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/features/next-one", "responses": [ledger, runs, projects, fields, saved], "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn the_description_is_a_textarea_with_a_write_and_preview_switch() {
    let Some(out) = edit(vec![]) else { return };
    assert_eq!(count(last(&out), "textarea[data-field-description]"), 1, "{out}");
    assert_eq!(count(last(&out), "input[data-field-description]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-description-preview]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-description-rendered]"), 0, "Write is the default: {out}");
}

#[test]
fn preview_renders_the_typed_markdown_and_write_brings_the_text_back() {
    let Some(out) = edit(vec![json!({"set": "[data-field-description]", "value": TYPED}), json!({"click": "[data-description-preview]"})]) else { return };
    assert_eq!(count(last(&out), "[data-description-rendered] strong"), 1, "{out}");
    assert_eq!(count(last(&out), "textarea[data-field-description]"), 0, "{out}");
    let Some(out) = edit(vec![json!({"set": "[data-field-description]", "value": TYPED}), json!({"click": "[data-description-preview]"}), json!({"click": "[data-description-write]"})]) else { return };
    assert_eq!(count(last(&out), "[data-description-rendered]"), 0, "{out}");
    assert_eq!(count(last(&out), "textarea[data-field-description]"), 1, "{out}");
}

#[test]
fn the_saved_description_equals_the_typed_text_line_breaks_included() {
    let steps = vec![json!({"set": "[data-field-description]", "value": TYPED}), json!({"click": "[data-description-preview]"}), json!({"click": "[data-feature-save]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = edit(steps) else { return };
    let sent: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(sent.len(), 1, "{out}");
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["fields"]["description"], TYPED, "{out}");
}
