//! The Todos entry in the sidebar and the phone tab bar carries a small badge with the number of Todo items. No
//! badge for an empty or unreadable list, none on the collapsed icon rail, and on a phone only a count of at most
//! two digits. The count follows an add without a page reload, and the link's name reads it ("Todos, 3 items").

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SIDEBAR_BADGE: &str = ".sidebar nav a[href=\"/todos\"] > .todo-count[data-todo-count]";
const TAB_BADGE: &str = "nav.tabbar a[href=\"/todos\"] > .todo-count[data-todo-count]";

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

/// The Dashboard after signing in, with the Todo list answered by `todo` in turn; `phone` lays it out at phone width.
fn run(phone: bool, todo: Vec<Value>, steps: Vec<Value>, selectors: Vec<String>) -> Option<Value> {
    run_at("/dashboard", vec![projects()], phone, todo, steps, selectors)
}

/// As `run`, starting at `url` and answering the pages' other reads with `responses` in turn.
fn run_at(url: &str, responses: Vec<Value>, phone: bool, todo: Vec<Value>, steps: Vec<Value>, selectors: Vec<String>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": url, "phone": phone, "responses": responses, "todo": todo, "steps": all, "selectors": selectors});
    let output = Command::new("node")
        .arg(format!("{dir}/tests/web_react_harness.mjs"))
        .arg(format!("{dir}/web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector]
        .as_u64()
        .unwrap_or_else(|| panic!("{selector} not probed: {frame}"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

/// The badge reading `n` in the sidebar, and the Todos link named with it.
fn sidebar_reads(n: i64) -> [String; 2] {
    let noun = if n == 1 { "item" } else { "items" };
    [
        format!("{SIDEBAR_BADGE}[data-todo-count=\"{n}\"]"),
        format!(".sidebar nav a[href=\"/todos\"][aria-label=\"Todos, {n} {noun}\"]"),
    ]
}

#[test]
fn the_todos_row_shows_the_item_count_when_the_sidebar_is_expanded() {
    let [badge, name] = sidebar_reads(3);
    let Some(out) = run(
        false,
        vec![todos(3)],
        vec![],
        vec![SIDEBAR_BADGE.into(), badge.clone(), name.clone()],
    ) else {
        return;
    };
    let frame = last(&out);
    assert_eq!(count(frame, SIDEBAR_BADGE), 1, "{out}");
    assert_eq!(count(frame, &badge), 1, "the badge reads 3: {out}");
    assert_eq!(
        count(frame, &name),
        1,
        "the link is named Todos, 3 items: {out}"
    );
}

#[test]
fn no_badge_for_zero_items_or_an_unreadable_list() {
    let unnamed = ".sidebar nav a[href=\"/todos\"]:not([aria-label])".to_string();
    for reply in [todos(0), json!({"status": 500, "json": {"error": "boom"}})] {
        let Some(out) = run(
            false,
            vec![reply],
            vec![],
            vec![SIDEBAR_BADGE.into(), unnamed.clone()],
        ) else {
            return;
        };
        let frame = last(&out);
        assert_eq!(count(frame, SIDEBAR_BADGE), 0, "{out}");
        assert_eq!(
            count(frame, &unnamed),
            1,
            "the link keeps its plain name: {out}"
        );
    }
}

#[test]
fn the_collapsed_rail_hides_the_count() {
    let Some(out) = run(
        false,
        vec![todos(3)],
        vec![json!({"nav": "[data-rail-button]"})],
        vec![SIDEBAR_BADGE.into(), ".shell.collapsed".into()],
    ) else {
        return;
    };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SIDEBAR_BADGE), 1, "expanded first: {out}");
    let frame = last(&out);
    assert_eq!(count(frame, ".shell.collapsed"), 1, "{out}");
    assert_eq!(count(frame, SIDEBAR_BADGE), 0, "{out}");
}

#[test]
fn the_phone_tab_shows_a_two_digit_count_and_hides_a_three_digit_one() {
    let twelve = format!("{TAB_BADGE}[data-todo-count=\"12\"]");
    let Some(out) = run(
        true,
        vec![todos(12)],
        vec![],
        vec![TAB_BADGE.into(), twelve.clone()],
    ) else {
        return;
    };
    assert_eq!(count(last(&out), &twelve), 1, "{out}");
    let Some(out) = run(true, vec![todos(100)], vec![], vec![TAB_BADGE.into()]) else {
        return;
    };
    assert_eq!(count(last(&out), TAB_BADGE), 0, "{out}");
}

#[test]
fn adding_a_todo_updates_the_count_without_a_reload() {
    let [two, _] = sidebar_reads(2);
    let [three, named] = sidebar_reads(3);
    let steps = vec![
        json!({"click": "[data-dashboard-add]"}),
        json!({"set": "[data-todo-name]", "value": "idea 3"}),
        json!({"click": "[data-todo-save]"}),
    ];
    let added = json!({"status": 200, "json": {"todo": item(3)}});
    let Some(out) = run(
        false,
        vec![todos(2), added, todos(3)],
        steps,
        vec![two.clone(), three.clone(), named.clone()],
    ) else {
        return;
    };
    assert_eq!(count(&out["frames"][1], &two), 1, "{out}");
    let frame = last(&out);
    assert_eq!(count(frame, &three), 1, "one more after the add: {out}");
    assert_eq!(count(frame, &named), 1, "{out}");
    let pages: Vec<&str> = out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["url"].as_str())
        .collect();
    assert_eq!(
        pages,
        ["/api/projects"],
        "the page was not loaded again: {out}"
    );
    assert_eq!(frame["settled"]["path"], "/dashboard", "{out}");
}

#[test]
fn coming_to_the_todos_or_dashboard_page_reads_the_list_again_and_recovers_from_a_failed_read() {
    let [three, _] = sidebar_reads(3);
    let [five, _] = sidebar_reads(5);
    let tile = "[data-dashboard-add][aria-label=\"5 Todos, add feature\"]".to_string();
    let selectors = vec![SIDEBAR_BADGE.into(), three.clone(), five.clone(), ".todo-item".into(), tile.clone()];
    let inbox = json!({"status": 200, "json": {"reviews": []}});
    let failed = json!({"status": 500, "json": {"error": "boom"}});
    let steps = vec![json!({"nav": ".sidebar nav a[href=\"/todos\"]"}), json!({"nav": ".sidebar nav a[href=\"/dashboard\"]"})];
    let Some(out) = run_at("/reviews", vec![inbox, projects(), projects()], false, vec![failed, todos(3), todos(5)], steps, selectors) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SIDEBAR_BADGE), 0, "no count while the list cannot be read: {out}");
    assert_eq!((frames[2]["settled"]["path"].as_str(), count(&frames[2], &three), count(&frames[2], ".todo-item")), (Some("/todos"), 1, 3), "the Todos page read it again: {out}");
    assert_eq!((frames[3]["settled"]["path"].as_str(), count(&frames[3], &five), count(&frames[3], &tile)), (Some("/dashboard"), 1, 1), "so did the Dashboard: {out}");
    let reads: Vec<&str> = out["todoRequests"].as_array().unwrap().iter().map(|r| r["method"].as_str().unwrap_or("GET")).collect();
    assert_eq!(reads, ["GET", "GET", "GET"], "one read per page: {out}");
}
