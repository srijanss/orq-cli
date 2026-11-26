//! The Dashboard's fourth tile (the Todo count) opens Add feature: a popup whose Project choice starts at "Todo (no project)",
//! which keeps a name and a description on the Todo list, or names a project, which opens the full feature
//! editor and saves a draft there. The Todos page holds the Todo list, newest first, ten at a time, each
//! item with Move to project, Edit and Delete. A draft moved with "Queue once approved" says so on its page,
//! with a Cancel.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 12] = [
    ".stats > [data-dashboard-add]",
    ".feature-modal [data-add-target]",
    ".todo-item",
    "[data-todo-save]:disabled",
    "[data-feature-save]:disabled",
    ".feature-modal",
    "[data-todo-show-all]",
    "[data-queue-wish-cancel]",
    "[data-field-name]",
    ".feature-modal [data-add-target]:disabled",
    "[data-modal-close]:disabled",
    "[data-dashboard-add][aria-label=\"1 Todo, add feature\"]",
];

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("beta"), project("alpha")]}})
}

fn ledger(actions: bool) -> Value {
    json!({"status": 200, "json": {"features": [{"name": "f1", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": actions}})
}

fn item(id: i64, name: &str, at: i64) -> Value {
    json!({"id": id, "name": name, "description": format!("about {name}"), "created_at": at, "updated_at": at, "version": 3})
}

fn todos(items: Vec<Value>, actions: bool) -> Value {
    json!({"status": 200, "json": {"todos": items, "remote_actions": actions}})
}

fn run(url: &str, responses: Vec<Value>, todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": url, "responses": responses, "todo": todo, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn has(frame: &Value, text: &str) -> bool {
    texts(frame).iter().any(|t| t == text)
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

/// The POSTs the page sent, Todo and queue-wish ones included, as (url, body).
fn posts(out: &Value) -> Vec<(String, Value)> {
    let all = out["requests"].as_array().unwrap().iter().chain(out["todoRequests"].as_array().unwrap());
    all.filter(|r| r["method"] == "POST").map(|r| (r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap()).unwrap())).collect()
}

#[test]
fn the_fourth_tile_opens_add_feature_and_an_empty_todo_list_says_so_on_the_todos_page() {
    let Some(out) = run("/dashboard", vec![projects()], vec![todos(vec![], true)], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[0]), 1, "{out}");
    assert!(!has(frame, "Last updated"), "the tile replaces Last updated: {out}");
    let Some(out) = run("/todos", vec![projects()], vec![todos(vec![], true)], vec![]) else { return };
    let shown = texts(last(&out));
    let todo = shown.iter().rposition(|t| t == "Todos").expect("a Todos heading");
    let empty = shown.iter().position(|t| t == "Nothing to do. Add feature puts ideas here.").expect("the empty text");
    assert!(todo < empty, "{shown:?}");
}

#[test]
fn todo_is_the_first_choice_and_the_projects_follow_by_name() {
    let Some(out) = run("/dashboard", vec![projects()], vec![todos(vec![], true)], vec![json!({"click": "[data-dashboard-add]"})]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[1]), 1, "{out}");
    let shown = texts(frame);
    let at = |text: &str| shown.iter().position(|t| t == text).unwrap_or_else(|| panic!("{text}: {shown:?}"));
    assert!(at("Todo (no project)") < at("alpha") && at("alpha") < at("beta"), "{shown:?}");
}

#[test]
fn adding_to_todo_posts_the_name_and_description_and_says_where_it_went() {
    let added = json!({"status": 200, "json": {"todo": item(7, "Fix the thing", 100)}});
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"set": "[data-todo-name]", "value": "Fix the thing"}),
        json!({"set": "[data-todo-description]", "value": "It breaks"}),
        json!({"click": "[data-todo-save]"}),
    ];
    let Some(out) = run("/dashboard", vec![projects()], vec![todos(vec![], true), added, todos(vec![item(7, "Fix the thing", 100)], true)], steps) else { return };
    assert_eq!(posts(&out), [("/api/todo".to_string(), json!({"name": "Fix the thing", "description": "It breaks"}))], "{out}");
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[5]), 0, "the popup closed: {out}");
    assert!(has(frame, "Added to Todo"), "{out}");
    assert_eq!(count(frame, SELECTORS[11]), 1, "the tile counts the new item: {out}");
}

#[test]
fn adding_to_a_project_saves_a_draft_through_the_feature_editor_with_hyphens_for_spaces() {
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"click": "[data-add-target]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
        json!({"set": "[data-field-name]", "value": "my new idea"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let saved = json!({"status": 200, "json": {"feature": "my-new-idea"}});
    let Some(out) = run("/dashboard", vec![projects(), ledger(true), saved], vec![todos(vec![], true)], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[4], SELECTORS[8]), 1, "the editor opened for alpha: {out}");
    assert!(has(&frames[5], "Saved as my-new-idea"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0].0, "/api/projects/alpha/features", "{out}");
    assert_eq!(sent[0].1["fields"]["name"], "my-new-idea", "{out}");
    assert!(has(last(&out), "Added to alpha as a draft"), "{out}");
    assert_eq!(count(last(&out), SELECTORS[5]), 0, "{out}");
}

#[test]
fn while_a_project_save_is_in_flight_the_project_cannot_be_changed_nor_the_popup_closed() {
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"click": "[data-add-target]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
        json!({"set": "[data-field-name]", "value": "idea"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
        json!({"keydown": "Escape"}),
    ];
    let held = json!({"status": 200, "json": {"feature": "idea"}, "hold": "fetch"});
    let Some(out) = run("/dashboard", vec![projects(), ledger(true), held], vec![todos(vec![], true)], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[4], SELECTORS[9]), 0, "the choice is open before the save: {out}");
    assert_eq!(count(&frames[7], SELECTORS[9]), 1, "{out}");
    assert_eq!(count(&frames[7], SELECTORS[10]), 1, "{out}");
    assert_eq!(count(last(&out), SELECTORS[5]), 1, "Escape does not close it mid-save: {out}");
}

#[test]
fn with_remote_actions_off_save_is_held_back_and_the_reason_shown() {
    let reason = "Remote actions are off, so nothing can be added. Turn them on in Settings.";
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"set": "[data-todo-name]", "value": "x"}),
        json!({"click": "[data-add-target]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
    ];
    let Some(out) = run("/dashboard", vec![projects(), ledger(false)], vec![todos(vec![], false)], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[3], SELECTORS[3]), 1, "Todo's Save: {out}");
    assert!(has(&frames[3], reason), "{out}");
    assert_eq!(count(&frames[5], SELECTORS[4]), 1, "the project's Save: {out}");
    assert!(has(&frames[5], reason), "{out}");
}

#[test]
fn more_than_ten_items_show_ten_until_show_all() {
    let items: Vec<Value> = (1..=12).rev().map(|n| item(n, &format!("idea {n}"), n * 10)).collect();
    let Some(out) = run("/todos", vec![projects()], vec![todos(items, true)], vec![json!({"click": "[data-todo-show-all]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[2]), 10, "{out}");
    assert!(has(&frames[1], "Show all (12)"), "{out}");
    assert!(has(&frames[1], "about idea 12"), "the description shows: {out}");
    assert_eq!(count(&frames[2], SELECTORS[2]), 12, "{out}");
}

#[test]
fn an_item_is_edited_with_its_version_and_deleted_after_asking() {
    let one = item(7, "one", 100);
    let edited = json!({"status": 200, "json": {"todo": item(7, "one better", 100)}});
    let deleted = json!({"status": 200, "json": {"deleted": 7}});
    let steps = vec![
        json!({"click": "[data-todo-edit]"}),
        json!({"set": ".feature-modal [data-todo-name]", "value": "one better"}),
        json!({"click": ".feature-modal [data-todo-save]"}),
        json!({"click": "[data-todo-delete]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let todo = vec![todos(vec![one], true), edited, todos(vec![item(7, "one better", 100)], true), deleted, todos(vec![], true)];
    let Some(out) = run("/todos", vec![projects()], todo, steps) else { return };
    assert!(has(&out["frames"][5], "Delete this Todo item?"), "{out}");
    assert_eq!(
        posts(&out),
        [("/api/todo/7".to_string(), json!({"name": "one better", "description": "about one", "version": 3})), ("/api/todo/7/delete".to_string(), json!({}))],
        "{out}"
    );
    assert_eq!(count(last(&out), SELECTORS[2]), 0, "{out}");
}

#[test]
fn move_to_project_prefills_the_editor_and_can_ask_for_the_draft_to_be_queued_once_approved() {
    let moved = json!({"status": 200, "json": {"feature": "Fix-the-thing", "project": "alpha"}});
    let steps = vec![
        json!({"click": "[data-todo-move]"}),
        json!({"click": "[data-add-target]"}),
        json!({"click": ".feature-modal [data-project-choice=alpha]"}),
        json!({"click": "[data-queue-when-approved]"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let todo = vec![todos(vec![item(7, "Fix the thing", 100)], true), moved, todos(vec![], true)];
    let Some(out) = run("/todos", vec![projects(), ledger(true)], todo, steps) else { return };
    assert!(!has(&out["frames"][2], "Todo (no project)"), "a move goes to a project: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0].0, "/api/todo/7/move", "{out}");
    let body = &sent[0].1;
    assert_eq!((&body["project"], &body["version"], &body["queue_when_approved"]), (&json!("alpha"), &json!(3), &json!(true)), "{out}");
    assert_eq!((&body["fields"]["name"], &body["fields"]["description"]), (&json!("Fix-the-thing"), &json!("about Fix the thing")), "{out}");
    assert!(has(last(&out), "Moved to alpha as a draft"), "{out}");
    assert_eq!(count(last(&out), SELECTORS[2]), 0, "{out}");
}

#[test]
fn a_draft_to_be_queued_once_approved_says_so_and_the_wish_can_be_cancelled() {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    let ledger = json!({"status": 200, "json": {"features": [feature("sketch", "draft")], "remote_actions": true}});
    let runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}});
    // The app reads the Todo list first on every page (the sidebar's count), then the draft's queue wish.
    let todo = vec![json!({"status": 200, "json": {"todos": [], "remote_actions": true}}), json!({"status": 200, "json": {"queue_when_approved": true}}), json!({"status": 200, "json": {"queue_when_approved": false}})];
    let Some(out) = run("/projects/alpha/features/sketch", vec![ledger, runs, projects()], todo, vec![json!({"click": "[data-queue-wish-cancel]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(has(&frames[1], "Will be queued when approved"), "{out}");
    assert_eq!(posts(&out), [("/api/projects/alpha/features/sketch/queue-wish".to_string(), json!({"queue_when_approved": false}))], "{out}");
    assert!(!has(last(&out), "Will be queued when approved"), "{out}");
    assert_eq!(count(last(&out), SELECTORS[7]), 0, "{out}");
}
