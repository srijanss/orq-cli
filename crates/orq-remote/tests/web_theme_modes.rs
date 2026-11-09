//! The theme has three modes (light, system, dark) in one pill, the choice is kept on this device
//! and restored on the next visit, and "system" follows the device setting.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn the_pill_holds_a_sun_a_monitor_and_a_moon_and_starts_on_system() {
    let sel = json!(["[data-theme-toggle] .theme-option", "[data-theme-toggle] .theme-option[data-active]", "[data-theme-toggle] .theme-option[data-active][data-mode=\"system\"]"]);
    let Some(out) = run(json!({"url": "/dashboard", "responses": [], "steps": [], "selectors": sel})) else { return };
    let m = &out["frames"][0]["settled"]["matches"];
    assert_eq!(m["[data-theme-toggle] .theme-option"], 3, "{out}");
    assert_eq!(m["[data-theme-toggle] .theme-option[data-active]"], 1, "{out}");
    assert_eq!(m["[data-theme-toggle] .theme-option[data-active][data-mode=\"system\"]"], 1, "{out}");
    assert_eq!(out["frames"][0]["settled"]["theme"], "", "system sets no explicit theme: {out}");
}

#[test]
fn each_click_moves_light_system_dark_and_system_clears_the_explicit_theme() {
    let steps = json!([{"nav": "[data-theme-toggle]"}, {"nav": "[data-theme-toggle]"}, {"nav": "[data-theme-toggle]"}]);
    let Some(out) = run(json!({"url": "/dashboard", "responses": [], "steps": steps})) else { return };
    let themes: Vec<&Value> = out["frames"].as_array().unwrap().iter().map(|f| &f["settled"]["theme"]).collect();
    assert_eq!(themes, ["", "dark", "light", ""], "{out}");
}

#[test]
fn the_choice_is_kept_in_local_storage_and_nothing_is_sent() {
    let Some(out) = run(json!({"url": "/dashboard", "responses": [], "steps": [{"nav": "[data-theme-toggle]"}]})) else { return };
    assert!(out["frames"][0]["settled"]["localTheme"].is_null(), "{out}");
    assert_eq!(out["frames"][1]["settled"]["localTheme"], "dark", "{out}");
    assert_eq!(out["requests"].as_array().unwrap().len(), 0, "{out}");
}

#[test]
fn a_kept_choice_is_applied_on_the_next_visit_and_shown_as_active() {
    let sel = json!(["[data-theme-toggle] .theme-option[data-active][data-mode=\"dark\"]"]);
    let Some(out) = run(json!({"url": "/dashboard", "local": {"orq.theme": "dark"}, "responses": [], "steps": [], "selectors": sel})) else { return };
    assert_eq!(out["frames"][0]["settled"]["theme"], "dark", "{out}");
    assert_eq!(out["frames"][0]["settled"]["matches"]["[data-theme-toggle] .theme-option[data-active][data-mode=\"dark\"]"], 1, "{out}");
}

#[test]
fn an_unknown_kept_value_is_ignored_and_blocked_storage_does_not_break_the_toggle() {
    let Some(out) = run(json!({"url": "/dashboard", "local": {"orq.theme": "purple"}, "responses": [], "steps": []})) else { return };
    assert_eq!(out["frames"][0]["settled"]["theme"], "", "{out}");
    let Some(out) = run(json!({"url": "/dashboard", "storage_blocked": true, "responses": [], "steps": [{"nav": "[data-theme-toggle]"}]})) else { return };
    assert_eq!(out["frames"][1]["settled"]["theme"], "dark", "{out}");
    let _ = TOKEN;
}
