//! The sidebar keeps a project dropdown at every place, stays in view while the page scrolls, shows
//! dividers, and lays its bottom items out like the reference.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
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

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

#[test]
fn outside_a_project_the_dropdown_offers_all_projects_and_loads_the_list_when_it_is_used() {
    let Some(out) = run("/dashboard", json!([projects(), projects()]), json!([{"submit": TOKEN}, {"nav": "[data-project-switcher]"}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["switchers"], json!([{"value": "", "options": [""]}]), "{out}");
    assert_eq!(out["frames"][2]["settled"]["switchers"], json!([{"value": "", "options": ["", "alpha", "beta"]}]), "{out}");
}

#[test]
fn choosing_a_project_opens_the_section_being_viewed_for_that_project() {
    let reviews = json!({"status": 200, "json": {"reviews": []}});
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-project-switcher]"}, {"select": "beta"}]);
    let Some(out) = run("/reviews", json!([projects(), reviews.clone(), reviews.clone(), projects(), reviews, projects()]), steps) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(last["settled"]["path"], "/projects/beta/reviews", "{out}");
}

#[test]
fn choosing_all_projects_inside_a_project_returns_to_the_all_projects_page_of_that_section() {
    let reviews = json!({"status": 200, "json": {"reviews": []}});
    let steps = json!([{"submit": TOKEN}, {"select": ""}]);
    let Some(out) = run("/projects/alpha/reviews", json!([reviews.clone(), projects(), projects(), reviews.clone(), reviews]), steps) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(last["settled"]["path"], "/reviews", "{out}");
}

#[test]
fn the_sidebar_stays_in_view_and_only_the_middle_scrolls() {
    let css = css();
    let side = rule(&css, ".sidebar {");
    assert!(side.contains("position: sticky") && side.contains("top: 0") && side.contains("height: 100dvh"), "{side}");
    let nav = rule(&css, ".sidebar nav {");
    assert!(nav.contains("overflow-y: auto") && nav.contains("min-height: 0"), "{nav}");
    assert!(rule(&css, ".sidebar-foot {").contains("flex: none"), "the bottom items never shrink out of view");
}

#[test]
fn dividers_separate_the_sections_from_settings_and_the_bottom_items() {
    let Some(out) = run("/dashboard", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1]["settled"];
    let seps = frame["classes"].as_array().unwrap();
    assert!(seps.iter().any(|c| c == "sidebar-divider"), "{out}");
    let css = css();
    assert!(rule(&css, ".sidebar-divider {").contains("var(--sidebar-divider)"));
    assert_eq!(css.matches("--sidebar-divider:").count(), 4, "one in each token block");
}

#[test]
fn the_bottom_items_are_a_theme_row_and_a_sign_out_row_that_fills_the_width() {
    let css = css();
    assert!(rule(&css, ".sidebar-foot {").contains("flex-direction: column"));
    assert!(rule(&css, ".sign-out {").contains("width: 100%"));
}
