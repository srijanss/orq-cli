//! The sidebar's project chooser is a compact custom dropdown (not a native select), and the icons
//! sit centred when the sidebar is collapsed to the rail.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value, selectors: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

#[test]
fn the_project_chooser_is_a_button_with_a_listbox_not_a_native_select() {
    let sel = json!(["select", "[data-project-switcher][aria-haspopup=\"listbox\"]", "[data-project-list][role=\"listbox\"]", "[data-project-option][role=\"option\"]"]);
    let Some(out) = run("/dashboard", json!([projects()]), json!([{"submit": TOKEN}]), sel) else { return };
    let m = &out["frames"][1]["settled"]["matches"];
    assert_eq!(m["select"], 0, "{out}");
    assert_eq!(m["[data-project-switcher][aria-haspopup=\"listbox\"]"], 1, "{out}");
    assert_eq!(m["[data-project-list][role=\"listbox\"]"], 1, "{out}");
    assert_eq!(m["[data-project-option][role=\"option\"]"], 1, "only All projects before the list loads: {out}");
}

#[test]
fn the_list_opens_on_a_click_marks_the_chosen_project_and_closes_on_a_second_click() {
    let sel = json!(["[data-project-list]:not([hidden])", "[data-project-option][aria-selected=\"true\"]", "[data-project-switcher][aria-expanded=\"true\"]"]);
    let Some(out) = run("/dashboard", json!([projects(), projects()]), json!([{"submit": TOKEN}, {"nav": "[data-project-switcher]"}, {"nav": "[data-project-switcher]"}]), sel) else { return };
    let open = |i: usize, k: &str| out["frames"][i]["settled"]["matches"][k].clone();
    assert_eq!(open(1, "[data-project-list]:not([hidden])"), 0, "{out}");
    assert_eq!(open(2, "[data-project-list]:not([hidden])"), 1, "{out}");
    assert_eq!(open(2, "[data-project-switcher][aria-expanded=\"true\"]"), 1, "{out}");
    assert_eq!(open(2, "[data-project-option][aria-selected=\"true\"]"), 1, "{out}");
    assert_eq!(open(3, "[data-project-list]:not([hidden])"), 0, "{out}");
}

#[test]
fn escape_and_an_outside_click_close_the_list() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/shell.jsx")).unwrap();
    assert!(source.contains("\"Escape\"") && source.contains("pointerdown"), "the list closes on Escape and on a press outside");
}

#[test]
fn the_list_floats_over_the_page_and_is_compact() {
    let css = css();
    let start = css.find(".project-list {").unwrap();
    let rule = &css[start..start + css[start..].find('}').unwrap()];
    assert!(rule.contains("position: absolute") && rule.contains("z-index"), "{rule}");
    let start = css.find("\n.project-switcher {").unwrap();
    let rule = &css[start..start + css[start..].find('}').unwrap()];
    assert!(rule.contains("min-height: 38px"), "{rule}");
}

#[test]
fn in_the_collapsed_rail_the_icons_are_centred() {
    let css = css();
    for needle in [".shell.collapsed .sidebar nav a {", ".shell.collapsed .sign-out {", ".shell.collapsed .sidebar-foot {", ".shell.collapsed .sidebar-head {"] {
        let start = css.find(needle).unwrap_or_else(|| panic!("{needle} missing"));
        let rule = &css[start..start + css[start..].find('}').unwrap()];
        assert!(rule.contains("justify-content: center") || rule.contains("align-items: center"), "{needle}: {rule}");
    }
    let start = css.find(".shell.collapsed .sidebar nav a {").unwrap();
    assert!(css[start..start + 200].contains("padding: 0"), "no side padding pushes the icon off centre");
}
