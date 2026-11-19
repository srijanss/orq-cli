//! The Dashboard's popups (Add feature, Move to project, Draft with Claude and a Todo item's Edit) are one kind of
//! box at every width, with one spacing inside, and they choose the project with the app's own project picker
//! rather than a native select.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = [
    ".feature-modal select",
    ".feature-modal .project-switcher[data-add-target][data-value=\"\"][aria-expanded=false]",
    ".feature-modal .project-list[role=listbox] [data-project-choice][role=option]",
    ".feature-modal [data-field-name]",
    ".feature-modal [data-project-choice=\"\"][aria-selected=true]",
    ".feature-modal .project-switcher[data-add-target][data-value=alpha]",
    ".feature-modal",
    ".feature-modal [data-add-target][aria-expanded=true]",
    ".feature-modal .project-list:not([hidden])",
];

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("beta"), project("alpha")]}})
}

fn ledger() -> Value {
    json!({"status": 200, "json": {"features": [{"name": "f1", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": true}})
}

fn item(id: i64, name: &str) -> Value {
    json!({"id": id, "name": name, "description": format!("about {name}"), "created_at": 100, "updated_at": 100, "version": 3})
}

fn todos(items: Vec<Value>) -> Value {
    json!({"status": 200, "json": {"todos": items, "remote_actions": true}})
}

/// The built page in jsdom with scripted replies; `None` when the web dependencies are not installed.
fn run(url: &str, responses: Vec<Value>, todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run_with(url, responses, todo, steps, &SELECTORS)
}

/// As `run`, counting `selectors` in each frame.
fn run_with(url: &str, responses: Vec<Value>, todo: Vec<Value>, steps: Vec<Value>, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": url, "responses": responses, "todo": todo, "steps": all, "selectors": selectors});
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

fn frames(out: &Value) -> &Vec<Value> {
    out["frames"].as_array().unwrap()
}

#[test]
fn add_feature_chooses_the_project_with_the_project_picker_starting_at_todo() {
    let steps = vec![json!({"click": "[data-dashboard-add]"}), json!({"click": ".feature-modal [data-add-target]"}), json!({"click": ".feature-modal [data-project-choice=alpha]"})];
    let Some(out) = run("/dashboard", vec![projects(), ledger()], vec![todos(vec![])], steps) else { return };
    let opened = &frames(&out)[2];
    assert_eq!(count(opened, SELECTORS[0]), 0, "no native select: {out}");
    assert_eq!(count(opened, SELECTORS[1]), 1, "{out}");
    assert_eq!(count(opened, SELECTORS[2]), 3, "Todo, alpha and beta: {out}");
    assert_eq!(count(opened, SELECTORS[4]), 1, "Todo is the chosen one: {out}");
    let shown = texts(opened);
    assert_eq!(shown.iter().filter(|t| *t == "Todo (no project)").count(), 2, "on the button and in the list: {shown:?}");
    let chosen = frames(&out).last().unwrap();
    assert_eq!(count(chosen, SELECTORS[5]), 1, "{out}");
    assert_eq!(count(chosen, SELECTORS[3]), 1, "the feature editor opened for alpha: {out}");
}

#[test]
fn the_picker_opens_moves_and_chooses_from_the_keyboard_and_escape_closes_only_the_list() {
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"keydown": "ArrowDown", "on": ".feature-modal [data-add-target]"}),
        json!({"keydown": "ArrowDown"}),
        json!({"keydown": "Enter"}),
        json!({"click": ".feature-modal [data-add-target]"}),
        json!({"keydown": "Escape"}),
    ];
    let Some(out) = run("/dashboard", vec![projects(), ledger()], vec![todos(vec![])], steps) else { return };
    let all = frames(&out);
    assert_eq!(count(&all[3], SELECTORS[7]), 1, "Down opens the list: {out}");
    assert_eq!(count(&all[3], SELECTORS[8]), 1, "{out}");
    assert_eq!(count(&all[5], SELECTORS[5]), 1, "Down then Enter chose alpha: {out}");
    assert_eq!(count(&all[5], SELECTORS[8]), 0, "and closed the list: {out}");
    assert_eq!(count(&all[6], SELECTORS[8]), 1, "a click opens it again: {out}");
    let last = all.last().unwrap();
    assert_eq!(count(last, SELECTORS[8]), 0, "Escape closed the list: {out}");
    assert_eq!(count(last, SELECTORS[6]), 1, "but not the popup: {out}");
}

const EDIT: [&str; 7] = [
    ".feature-modal > .feature-modal-box[role=dialog][aria-modal=true]",
    ".feature-modal [data-todo-name]",
    ".todo-item .todo-edit, .todo-item [data-todo-name]",
    ".todo-item [data-todo-edit]",
    ".feature-modal .error-text",
    "[data-todo-item=\"7\"] [data-todo-edit]",
    ".feature-modal [data-modal-close]",
];

/// The POSTs the page sent to the Todo list, as (url, body).
fn todo_posts(out: &Value) -> Vec<(String, Value)> {
    let all = out["todoRequests"].as_array().unwrap();
    all.iter().filter(|r| r["method"] == "POST").map(|r| (r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap()).unwrap())).collect()
}

fn focused(frame: &Value, selector: &str) -> bool {
    frame["settled"]["focusMatches"][selector].as_bool().unwrap()
}

fn name_values(frame: &Value) -> Vec<String> {
    frame["settled"]["inputs"].as_array().unwrap().iter().filter(|i| i["type"] == "text").map(|i| i["value"].as_str().unwrap().to_string()).collect()
}

#[test]
fn edit_opens_an_edit_todo_popup_that_saves_with_the_version_or_keeps_the_error() {
    let edit = |todo: Vec<Value>, steps: Vec<Value>| run_with("/dashboard", vec![projects()], todo, steps, &EDIT);
    let saved = json!({"status": 200, "json": {"todo": item(7, "one better")}});
    let steps = vec![json!({"click": "[data-todo-edit]"}), json!({"set": ".feature-modal [data-todo-name]", "value": "one better"}), json!({"click": ".feature-modal [data-todo-save]"})];
    let Some(out) = edit(vec![todos(vec![item(7, "one")]), saved, todos(vec![item(7, "one better")])], steps) else { return };
    let all = frames(&out);
    let opened = &all[2];
    assert_eq!(count(opened, EDIT[0]), 1, "{out}");
    assert!(texts(opened).iter().any(|t| t == "Edit Todo"), "{out}");
    assert_eq!(name_values(opened), ["one"], "{out}");
    assert!(focused(opened, EDIT[1]), "the Name field has the focus: {out}");
    assert_eq!(count(opened, EDIT[2]), 0, "the item's card is not a form: {out}");
    assert_eq!(count(opened, EDIT[3]), 1, "the item's card is unchanged: {out}");
    assert_eq!(todo_posts(&out), [("/api/todo/7".to_string(), json!({"name": "one better", "description": "about one", "version": 3}))], "{out}");
    let last = all.last().unwrap();
    assert_eq!(count(last, EDIT[0]), 0, "closed: {out}");
    assert!(texts(last).iter().any(|t| t == "one better"), "the list shows the new name: {out}");

    let refused = json!({"status": 409, "json": {"error": "The item changed since it was read."}});
    let steps = vec![json!({"click": "[data-todo-edit]"}), json!({"set": ".feature-modal [data-todo-name]", "value": "typed"}), json!({"click": ".feature-modal [data-todo-save]"})];
    let Some(out) = edit(vec![todos(vec![item(7, "one")]), refused], steps) else { return };
    let last = frames(&out).last().unwrap();
    assert_eq!(count(last, EDIT[0]), 1, "stays open: {out}");
    assert!(texts(last).iter().any(|t| t == "The item changed since it was read."), "{out}");
    assert_eq!(name_values(last), ["typed"], "what was typed stays: {out}");
}

#[test]
fn escape_cancel_and_close_leave_the_edit_popup_unsaved_and_give_edit_the_focus_back() {
    for close in [json!({"keydown": "Escape"}), json!({"click": ".feature-modal [data-cancel]"}), json!({"click": ".feature-modal [data-modal-close]"})] {
        let steps = vec![json!({"click": "[data-todo-edit]"}), json!({"set": ".feature-modal [data-todo-name]", "value": "typed"}), close.clone()];
        let Some(out) = run_with("/dashboard", vec![projects()], vec![todos(vec![item(7, "one")])], steps, &EDIT) else { return };
        let last = frames(&out).last().unwrap();
        assert_eq!(count(last, EDIT[0]), 0, "{close} closed it: {out}");
        assert!(todo_posts(&out).is_empty(), "{close} saved nothing: {out}");
        assert!(focused(last, EDIT[5]), "{close} gave Edit the focus back: {out}");
    }
}

const PLAN_PICKER: &str = ".feature-modal .project-switcher[data-plan-project][data-value=\"\"] .project-switcher-text.muted";
const MOVE_PICKER: &str = ".feature-modal .project-switcher[data-add-target][data-value=\"\"] .project-switcher-text.muted";

#[test]
fn move_and_draft_with_claude_use_the_same_picker_waiting_on_a_project() {
    let one = || todos(vec![item(7, "dark mode")]);
    let selectors = [PLAN_PICKER, MOVE_PICKER, ".feature-modal select", "[data-plan-start]:disabled", ".feature-modal [data-plan-project][data-value=alpha]", "[data-feature-save]", ".feature-modal [data-project-choice]"];
    let scenario = |steps: Vec<Value>, todo: Vec<Value>| run_with("/dashboard", vec![projects(), ledger()], todo, steps, &selectors);
    let steps = vec![json!({"click": "[data-todo-draft]"}), json!({"click": ".feature-modal [data-plan-project]"}), json!({"click": ".feature-modal [data-project-choice=alpha]"})];
    let Some(out) = scenario(steps, vec![one()]) else { return };
    let all = frames(&out);
    assert_eq!(count(&all[2], PLAN_PICKER), 1, "{out}");
    assert!(texts(&all[2]).iter().any(|t| t == "Choose a project"), "{out}");
    assert_eq!(count(&all[2], ".feature-modal select"), 0, "{out}");
    assert_eq!(count(&all[2], ".feature-modal [data-project-choice]"), 2, "alpha and beta: {out}");
    assert_eq!(count(&all[2], "[data-plan-start]:disabled"), 1, "Start waits for a project: {out}");
    let last = all.last().unwrap();
    assert_eq!(count(last, ".feature-modal [data-plan-project][data-value=alpha]"), 1, "{out}");
    assert_eq!(count(last, "[data-plan-start]:disabled"), 0, "{out}");
    let Some(out) = scenario(vec![json!({"click": "[data-todo-move]"})], vec![one()]) else { return };
    let moved = frames(&out).last().unwrap();
    assert_eq!(count(moved, MOVE_PICKER), 1, "{out}");
    assert_eq!(count(moved, ".feature-modal select"), 0, "{out}");
    assert_eq!(count(moved, "[data-feature-save]"), 0, "nothing to save before a project: {out}");
    assert_eq!(count(moved, ".feature-modal [data-project-choice]"), 2, "{out}");
}

/// The Dashboard in Chromium at `width` with one Todo item, after pressing `open` and waiting for `wait`, then the
/// `measured` steps; `None` without a Chromium.
fn dashboard_in_chromium(width: u32, open: &str, wait: &str, measured: Value) -> Option<Value> {
    let chromium = web_chromium::binary()?;
    let replies = [
        web_chromium::get("/api/projects", projects()["json"].clone()),
        web_chromium::get("/api/todo", todos(vec![item(7, "dark mode")])["json"].clone()),
        web_chromium::get("/api/projects/alpha/features", ledger()["json"].clone()),
    ];
    let mut steps = vec![json!({"wait": "[data-todo-edit]"}), json!({"click": open}), json!({"wait": wait})];
    steps.extend(measured.as_array().unwrap().iter().cloned());
    let out = web_chromium::measure(&chromium, width, "/dashboard", &replies, Value::Array(steps));
    assert!(out.result.get("error").is_none(), "{}\nunanswered: {:?}", out.result, out.unanswered);
    Some(out.result)
}

#[test]
fn at_390_wide_add_draft_and_edit_are_popup_boxes_over_a_backdrop_without_sideways_scroll() {
    for (open, dialog) in [("[data-dashboard-add]", "[data-feature-modal]"), ("[data-todo-draft]", "[data-plan-dialog]"), ("[data-todo-edit]", "[data-todo-edit-dialog]")] {
        let shell = format!("{dialog} > .feature-modal-box");
        let head = format!("{dialog} .feature-modal-head");
        let close = format!("{dialog} [data-modal-close]");
        let measured = json!([{"boxes": dialog}, {"boxes": shell}, {"boxes": head}, {"boxes": close}, {"styles": shell, "props": ["overflow-y"]}]);
        let Some(result) = dashboard_in_chromium(390, open, &shell, measured) else { return };
        assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{open}: the page scrolls sideways: {result}");
        let backdrop = &result["boxes"][dialog][0];
        assert_eq!((backdrop["left"].as_i64(), backdrop["right"].as_i64()), (Some(0), Some(390)), "{open}: the backdrop covers the screen: {result}");
        let shown = &result["boxes"][&shell][0];
        assert_eq!((shown["left"].as_i64(), shown["right"].as_i64()), (Some(12), Some(378)), "{open}: the box less 0.75rem each side: {result}");
        for part in [&head, &close] {
            let b = &result["boxes"][part.as_str()][0];
            assert!(b["bottom"].as_i64().unwrap_or(0) > b["top"].as_i64().unwrap_or(0), "{open}: {part} shows: {result}");
        }
        assert_eq!(result["styles"][&shell][0]["overflow-y"], "auto", "{open}: the box scrolls inside itself: {result}");
    }
}

fn side(result: &Value, selector: &str, edge: &str) -> i64 {
    result["boxes"][selector][0][edge].as_i64().unwrap_or_else(|| panic!("{selector} has no box: {result}"))
}

#[test]
fn at_1024_wide_the_project_picker_lines_up_with_name_and_the_box_is_the_only_card() {
    const PROJECT: &str = "[data-feature-modal] .feature-modal-box > .field";
    const PICKER: &str = "[data-feature-modal] [data-add-target]";
    const NAME: &str = "[data-feature-modal] .feature-editor-fields > .field:has([data-todo-name])";
    const DESCRIPTION: &str = "[data-feature-modal] [data-todo-description]";
    const FIELDS: &str = "[data-feature-modal] .feature-editor-fields";
    let measured = json!([{"boxes": PROJECT}, {"boxes": PICKER}, {"boxes": NAME}, {"boxes": DESCRIPTION}, {"styles": FIELDS, "props": ["border-top-width", "padding-left", "padding-top", "background-color"]}]);
    let Some(result) = dashboard_in_chromium(1024, "[data-dashboard-add]", PICKER, measured) else { return };
    assert_eq!(side(&result, PICKER, "left"), side(&result, NAME, "left"), "Project and Name share a left edge: {result}");
    assert_eq!(side(&result, NAME, "top") - side(&result, PROJECT, "bottom"), 12, "0.75rem between Project and Name: {result}");
    assert_eq!(side(&result, PICKER, "right"), side(&result, DESCRIPTION, "right"), "the picker is full width: {result}");
    assert!(side(&result, PICKER, "bottom") - side(&result, PICKER, "top") >= 44, "the picker is 44px tall: {result}");
    let fields = &result["styles"][FIELDS][0];
    assert_eq!((fields["border-top-width"].as_str(), fields["padding-left"].as_str(), fields["padding-top"].as_str()), (Some("0px"), Some("0px"), Some("0px")), "{result}");
    assert_eq!(fields["background-color"], "rgba(0, 0, 0, 0)", "no card inside the box: {result}");
}

#[test]
fn a_failed_save_shows_its_status_half_a_rem_under_the_actions_and_the_features_page_editor_still_lies_flat() {
    let Some(chromium) = web_chromium::binary() else { return };
    let refused = web_chromium::Reply { method: "POST", path: "/api/todo/7".to_string(), status: 409, body: json!({"error": "The item changed since it was read."}) };
    let replies = [
        web_chromium::get("/api/projects", projects()["json"].clone()),
        web_chromium::get("/api/todo", todos(vec![item(7, "dark mode")])["json"].clone()),
        refused,
    ];
    const ACTIONS: &str = "[data-todo-edit-dialog] .feature-editor-actions";
    const STATUS: &str = "[data-todo-edit-dialog] .error-text";
    let steps = json!([{"click": "[data-todo-edit]"}, {"click": "[data-todo-edit-dialog] [data-todo-save]"}, {"wait": STATUS}, {"boxes": ACTIONS}, {"boxes": STATUS}]);
    let out = web_chromium::measure(&chromium, 1024, "/dashboard", &replies, steps);
    let result = &out.result;
    assert!(result.get("error").is_none(), "{result}\nunanswered: {:?}", out.unanswered);
    assert_eq!(side(result, STATUS, "top") - side(result, ACTIONS, "bottom"), 8, "{result}");

    let replies = [web_chromium::get("/api/projects", projects()["json"].clone()), web_chromium::get("/api/projects/alpha/features", ledger()["json"].clone())];
    const INLINE: &str = ".feature-filters .feature-modal-box";
    let steps = json!([
        {"click": "[data-feature-add]"},
        {"wait": ".feature-filters [data-field-name]"},
        {"styles": INLINE, "props": ["display"]},
        {"styles": ".feature-filters .feature-modal-head", "props": ["display"]},
        {"styles": ".feature-filters .feature-editor-fields", "props": ["border-top-width", "padding-left"]},
    ]);
    let out = web_chromium::measure(&chromium, 390, "/projects/alpha/features", &replies, steps);
    let result = &out.result;
    assert!(result.get("error").is_none(), "{result}\nunanswered: {:?}", out.unanswered);
    assert_eq!(result["styles"][INLINE][0]["display"], "contents", "{result}");
    assert_eq!(result["styles"][".feature-filters .feature-modal-head"][0]["display"], "none", "{result}");
    let fields = &result["styles"][".feature-filters .feature-editor-fields"][0];
    assert_eq!((fields["border-top-width"].as_str(), fields["padding-left"].as_str()), (Some("1px"), Some("16px")), "the inline editor keeps its card: {result}");
}
