//! A cog beside the "Project:" picker opens the chosen project's Settings page, or this machine's
//! Settings page while All projects is chosen. The Settings page itself has no cog: it is already there.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 4] = [
    "[data-project-select]",
    "[data-settings-link]",
    "a[data-settings-link][href=\"/projects/alpha/settings\"][aria-label=\"Project settings\"]",
    "a[data-settings-link][href=\"/settings\"][aria-label=\"Settings\"]",
];

fn project(id: &str) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn features() -> Value {
    json!({"status": 200, "json": {"features": [{"name": "f1", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}]}})
}

fn page(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps.as_array().unwrap().iter().cloned());
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn a_projects_page_has_a_cog_that_opens_the_projects_settings() {
    let Some(out) = page("/projects/alpha/features", json!([features(), projects(), {"status": 500}]), json!([{"nav": "[data-settings-link]"}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-settings-link]"), 1, "{out}");
    assert_eq!(count(frame, SELECTORS[2]), 1, "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/settings", "{out}");
}

#[test]
fn all_projects_has_a_cog_that_opens_this_machines_settings() {
    let empty = json!({"status": 200, "json": {"features": []}});
    let Some(out) = page("/features", json!([projects(), empty.clone(), empty, {"status": 500}]), json!([{"nav": "[data-settings-link]"}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-settings-link]"), 1, "{out}");
    assert_eq!(count(frame, SELECTORS[3]), 1, "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/settings", "{out}");
}

#[test]
fn the_settings_page_itself_has_no_cog() {
    let agent = json!({"project": null, "fallback": {"harness": "claude", "model": "opus", "effort": null, "source": "machine_default"}});
    let settings = json!({"status": 200, "json": {"settings": {"implementer": agent, "reviewer": agent, "delivery": "open_review", "revision": 1}}});
    let Some(out) = page("/projects/alpha/settings", json!([settings, projects()]), json!([])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(count(frame, "[data-project-select]"), 1, "the picker is there: {out}");
    assert_eq!(count(frame, "[data-settings-link]"), 0, "{out}");
}

fn css_rule(selector: &str) -> String {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    css.lines().find(|line| line.starts_with(&format!("{selector} {{"))).unwrap_or_else(|| panic!("a {selector} rule")).to_string()
}

/// The cog is the gear alone beside the picker: no box drawn around it, so no border, background or fixed size.
#[test]
fn the_cog_is_a_bare_icon_without_a_box() {
    let rule = css_rule(".project-settings-link");
    for boxed in ["border:", "border-radius", "background", "width:", "height:"] {
        assert!(!rule.contains(boxed), "{boxed} in {rule}");
    }
}
