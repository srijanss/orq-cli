//! The Dashboard's fourth tile counts the Todo items and still opens Add feature; the Todo Edit form is the
//! same card as Add; text areas take the inputs' design tokens; at phone width the tiles fit.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 11] = [
    ".stats > .stat",
    ".stats > [data-dashboard-add]",
    "[data-dashboard-add][aria-label=\"3 Todos, add feature\"]",
    "[data-dashboard-add][aria-label=\"1 Todo, add feature\"]",
    ".feature-modal",
    ".todo-item",
    "[data-dashboard-add][aria-label=\"2 Todos, add feature\"]",
    "[data-dashboard-add][title=\"The Todo list could not be read.\"]",
    ".todo-item .feature-editor-fields > .field > [data-todo-name]",
    ".todo-item .feature-editor-fields > .field-wide > [data-todo-description]",
    ".todo-item .feature-editor-fields > .feature-editor-actions > [data-todo-save].primary-button + [data-cancel]",
];

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

fn item(id: i64) -> Value {
    json!({"id": id, "name": format!("idea {id}"), "description": "", "created_at": id * 10, "updated_at": id * 10, "version": 1})
}

fn todos(count: i64) -> Value {
    json!({"status": 200, "json": {"todos": (1..=count).rev().map(item).collect::<Vec<_>>(), "remote_actions": true}})
}

fn run(todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/dashboard", "responses": [projects()], "todo": todo, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn has(frame: &Value, text: &str) -> bool {
    frame["settled"]["texts"].as_array().unwrap().iter().any(|t| t == text)
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn the_fourth_tile_counts_the_todo_items_and_there_are_still_four_tiles() {
    let Some(out) = run(vec![todos(3)], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[0]), 4, "{out}");
    assert_eq!(count(frame, SELECTORS[2]), 1, "{out}");
    assert!(has(frame, "3") && has(frame, "Todos"), "{out}");
    assert!(!has(frame, "Add feature"), "{out}");
}

#[test]
fn one_item_reads_one_todo_and_a_list_that_cannot_be_read_a_dash_with_the_reason() {
    let Some(out) = run(vec![todos(1)], vec![]) else { return };
    assert_eq!(count(last(&out), SELECTORS[3]), 1, "{out}");
    assert!(has(last(&out), "Todo"), "{out}");
    let Some(out) = run(vec![json!({"status": 500, "json": {"error": "boom"}})], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[7]), 1, "{out}");
    assert!(has(frame, "–"), "{out}");
    assert_eq!(count(frame, SELECTORS[0]), 4, "{out}");
}

#[test]
fn pressing_the_tile_opens_the_add_feature_popup() {
    let Some(out) = run(vec![todos(3)], vec![json!({"click": "[data-dashboard-add]"})]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[4]), 1, "{out}");
    assert!(has(frame, "Add feature"), "the popup's title: {out}");
}

#[test]
fn the_count_follows_an_add_and_a_delete_without_a_reload() {
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"set": "[data-todo-name]", "value": "idea 3"}),
        json!({"click": "[data-todo-save]"}),
    ];
    let added = json!({"status": 200, "json": {"todo": item(3)}});
    let Some(out) = run(vec![todos(2), added, todos(3)], steps) else { return };
    assert_eq!(count(&out["frames"][1], SELECTORS[6]), 1, "{out}");
    assert_eq!(count(last(&out), SELECTORS[2]), 1, "one more after the add: {out}");
    let deleted = json!({"status": 200, "json": {"deleted": 2}});
    let Some(out) = run(vec![todos(2), deleted, todos(1)], vec![json!({"click": "[data-todo-delete]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(count(&out["frames"][1], SELECTORS[6]), 1, "{out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 1, "one fewer after the delete: {out}");
}

#[test]
fn the_edit_form_is_the_feature_editor_card_with_its_actions_under_the_fields() {
    let Some(out) = run(vec![todos(1)], vec![json!({"click": "[data-todo-edit]"})]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[8]), 1, "Name: {out}");
    assert_eq!(count(frame, SELECTORS[9]), 1, "Description, full width: {out}");
    assert_eq!(count(frame, SELECTORS[10]), 1, "Save then Cancel: {out}");
}

/// The declarations of every rule in app.css whose selector list names `selector` on its own, joined.
fn declarations(selector: &str) -> String {
    let css = std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap();
    css.split('}')
        .filter_map(|rule| {
            let (head, body) = rule.rsplit_once('{')?;
            let head = head.rsplit_once("*/").map_or(head, |(_, after)| after);
            head.split(',').any(|s| s.trim() == selector).then(|| body.to_string())
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[test]
fn a_text_area_takes_the_inputs_background_border_and_radius_tokens() {
    let input = declarations("input");
    let textarea = declarations("textarea");
    for wanted in ["background: var(--surface)", "color: var(--text)", "border: 1px solid var(--border)", "border-radius: 0.5rem", "box-sizing: border-box"] {
        assert!(input.contains(wanted), "input has {wanted}: {input}");
        assert!(textarea.contains(wanted), "textarea has {wanted}: {textarea}");
    }
    for wanted in ["padding: 0.5rem 0.75rem", "resize: vertical", "min-height:"] {
        assert!(textarea.contains(wanted), "textarea has {wanted}: {textarea}");
    }
}

#[test]
fn at_390_wide_the_tiles_fit_and_the_todo_tile_is_at_least_44px_tall() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = [
        web_chromium::get("/api/projects", projects()["json"].clone()),
        web_chromium::get("/api/todo", todos(12)["json"].clone()),
    ];
    let steps = json!([{"wait": "[data-dashboard-add]", "text": "12Todos"}, {"boxes": ".stats > .stat"}, {"boxes": "[data-dashboard-add]"}]);
    let out = web_chromium::measure(&chromium, 390, "/dashboard", &replies, steps);
    let result = &out.result;
    assert!(result.get("error").is_none(), "{result}\nunanswered: {:?}", out.unanswered);
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "the page scrolls sideways: {result}");
    assert_eq!(result["boxes"][".stats > .stat"].as_array().unwrap().len(), 4, "{result}");
    let tile = &result["boxes"]["[data-dashboard-add]"][0];
    assert!(tile["bottom"].as_i64().unwrap() - tile["top"].as_i64().unwrap() >= 44, "{tile}");
    assert!(tile["right"].as_i64().unwrap() <= 390, "{tile}");
}
