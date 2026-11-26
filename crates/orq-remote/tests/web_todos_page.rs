//! The Todo list has its own page at /todos, reached from the sidebar and the phone tab bar right after
//! Dashboard. The Dashboard keeps only the Todos tile, which still counts the items and opens Add feature.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 8] = [
    ".todo-item",
    ".todo-item [data-todo-move]",
    ".todo-item [data-todo-draft]",
    ".todo-item [data-todo-edit]",
    ".todo-item [data-todo-delete]",
    "[data-todo]",
    "[data-dashboard-add][aria-label=\"3 Todos, add feature\"]",
    "[data-todo-show-all]",
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

fn run(url: &str, todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run_with(url, vec![projects()], todo, steps)
}

/// As `run`, answering the page's other reads with `responses` in turn.
fn run_with(url: &str, responses: Vec<Value>, todo: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
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

fn shell(scenario: Value) -> Option<Value> {
    web_deps::ready().then(|| {
        let dir = env!("CARGO_MANIFEST_DIR");
        let output = Command::new("node").arg(format!("{dir}/tests/web_shell_run.mjs")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
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
fn the_todos_page_lists_the_items_with_their_actions() {
    let Some(out) = run("/todos", vec![todos(3)], vec![]) else { return };
    let frame = last(&out);
    assert!(has(frame, "Todos"), "the page header: {out}");
    for selector in &SELECTORS[..5] {
        assert_eq!(count(frame, selector), 3, "{selector}: {out}");
    }
    assert_eq!(frame["settled"]["path"], "/todos", "{out}");
}

#[test]
fn the_dashboard_no_longer_shows_the_todo_list() {
    let Some(out) = run("/dashboard", vec![todos(3)], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, SELECTORS[5]), 0, "{out}");
    assert_eq!(count(frame, SELECTORS[0]), 0, "{out}");
    assert_eq!(count(frame, SELECTORS[6]), 1, "the tile still counts: {out}");
}

fn links(out: &Value) -> Vec<(String, String, Option<String>)> {
    out["frames"][0]["links"].as_array().unwrap().iter().map(|l| (l["text"].as_str().unwrap().to_string(), l["href"].as_str().unwrap().to_string(), l["current"].as_str().map(String::from))).collect()
}

#[test]
fn the_sidebar_offers_todos_after_dashboard() {
    let Some(out) = shell(json!({"path": "/todos"})) else { return };
    let links = links(&out);
    let pairs: Vec<(&str, &str)> = links.iter().map(|(text, href, _)| (text.as_str(), href.as_str())).collect();
    assert_eq!(pairs, [("Dashboard", "/dashboard"), ("Todos", "/todos"), ("Features", "/features"), ("Orchestrations", "/orchestrations"), ("Queue", "/queue"), ("Reviews", "/reviews"), ("Stats", "/stats"), ("Settings", "/settings")], "{out}");
    let current: Vec<&str> = links.iter().filter(|(_, _, current)| current.as_deref() == Some("page")).map(|(text, _, _)| text.as_str()).collect();
    assert_eq!(current, ["Todos"], "{out}");
}

#[test]
fn inside_a_project_todos_still_points_at_slash_todos() {
    let Some(out) = shell(json!({"path": "/projects/alpha/features", "projectId": "alpha"})) else { return };
    let links = links(&out);
    let todos = links.iter().find(|(text, _, _)| text == "Todos").unwrap_or_else(|| panic!("no Todos link: {out}"));
    assert_eq!((todos.1.as_str(), todos.2.as_deref()), ("/todos", None), "{out}");
    assert_eq!(links[1].0, "Todos", "right after Dashboard: {out}");
}

#[test]
fn the_phone_bar_shows_todos_first_in_its_second_row() {
    let second = "nav.tabbar > [data-tab-row=\"2\"] > a";
    let Some(out) = shell(json!({"path": "/todos", "phone": true, "probe": [second]})) else { return };
    let frame = &out["frames"][0];
    let texts: Vec<&str> = frame["probes"][second].as_array().unwrap().iter().map(|item| item["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["Todos", "Reviews", "Stats"], "{out}");
    assert_eq!(frame["probes"][second][0]["current"], "page", "{out}");
}

#[test]
fn show_all_and_delete_work_on_the_todos_page() {
    let deleted = json!({"status": 200, "json": {"deleted": 12}});
    let steps = vec![json!({"click": "[data-todo-show-all]"}), json!({"click": "[data-todo-delete]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/todos", vec![todos(12), deleted, todos(11)], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 10, "{out}");
    assert_eq!(count(&frames[1], SELECTORS[7]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[0]), 12, "{out}");
    let todo: Vec<(&str, &str)> = out["todoRequests"].as_array().unwrap().iter().map(|r| (r["method"].as_str().unwrap_or("GET"), r["url"].as_str().unwrap())).collect();
    assert_eq!(todo, [("GET", "/api/todo"), ("POST", "/api/todo/12/delete"), ("GET", "/api/todo")], "{out}");
    assert_eq!(count(last(&out), SELECTORS[0]), 11, "the list was read again: {out}");
}

#[test]
fn refresh_reads_the_todo_list_again_and_recovers_after_a_failed_read() {
    let failed = json!({"status": 500, "json": {"error": "boom"}});
    let Some(out) = run_with("/todos", vec![projects(), projects()], vec![failed, todos(3)], vec![json!({"nav": "[data-refresh]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(has(&frames[1], "The Todo list could not be read."), "{out}");
    assert_eq!(count(&frames[1], SELECTORS[0]), 0, "{out}");
    let reads: Vec<&str> = out["todoRequests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(reads, ["/api/todo", "/api/todo"], "Refresh reads the list again: {out}");
    let refreshed = last(&out);
    assert_eq!(count(refreshed, SELECTORS[0]), 3, "{out}");
    assert!(!has(refreshed, "The Todo list could not be read."), "{out}");
}

#[test]
fn todos_is_an_application_path() {
    let served = |path: &str| orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", path).map(|asset| asset.body);
    assert!(served("/dashboard").is_some());
    assert_eq!(served("/todos"), served("/dashboard"));
    assert_eq!(served("/todos/"), None);
}
