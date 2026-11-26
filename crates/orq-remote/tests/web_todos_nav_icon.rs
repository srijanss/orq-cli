//! The Todos entry in the sidebar and the phone tab bar shows the `listChecks` icon (ticks beside lines, a list of
//! things to do) rather than `check`, a plain tick that reads as "done". `check` stays on Save.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

/// The `listChecks` icon's path (Lucide's list-checks shape).
const LIST_CHECKS: &str = "m3 7 2 2 4-4m-6 8 2 2 4-4M13 6h8M13 12h8M13 18h8";
/// The `check` icon's path.
const CHECK: &str = "m5 12 5 5L20 7";

const SIDEBAR_TODOS: &str = ".sidebar nav a[href=\"/todos\"]";
const TAB_TODOS: &str = "nav.tabbar a[href=\"/todos\"]";

/// The app shell (built from source) at `path`, its Todos links probed.
fn shell(path: &str, phone: bool) -> Option<Value> {
    web_deps::ready().then(|| {
        let dir = env!("CARGO_MANIFEST_DIR");
        let scenario = json!({"path": path, "projectId": "demo", "phone": phone, "probe": [SIDEBAR_TODOS, TAB_TODOS], "steps": []});
        let output = Command::new("node").arg(format!("{dir}/tests/web_shell_run.mjs")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

fn only<'a>(out: &'a Value, selector: &str) -> &'a Value {
    let found = out["frames"][0]["probes"][selector].as_array().unwrap_or_else(|| panic!("no probe for {selector}: {out}"));
    assert_eq!(found.len(), 1, "one {selector}: {out}");
    &found[0]
}

#[test]
fn the_todos_sidebar_link_shows_the_list_checks_icon() {
    let Some(out) = shell("/dashboard", false) else { return };
    let link = only(&out, SIDEBAR_TODOS);
    assert_eq!(link["icon"], LIST_CHECKS, "{out}");
    assert_ne!(link["icon"], CHECK, "{out}");
}

#[test]
fn the_todos_phone_tab_shows_the_list_checks_icon_at_size_28() {
    let Some(out) = shell("/todos", true) else { return };
    let tab = only(&out, TAB_TODOS);
    assert_eq!(tab["icon"], LIST_CHECKS, "{out}");
    assert_eq!(tab["iconSize"], "28", "{out}");
}

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SAVE_CHECK: &str = "[data-todo-save] svg path[d=\"m5 12 5 5L20 7\"]";

#[test]
fn a_save_button_still_shows_the_check_icon() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let projects = json!({"status": 200, "json": {"projects": [project]}});
    let todos = json!({"status": 200, "json": {"todos": [], "remote_actions": true}});
    let steps = [json!({"submit": TOKEN}), json!({"click": "[data-dashboard-add]"})];
    let scenario = json!({"url": "/dashboard", "responses": [projects], "todo": [todos], "steps": steps, "selectors": [SAVE_CHECK]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(last["settled"]["matches"][SAVE_CHECK], 1, "the Save button draws check's path: {out}");
}
