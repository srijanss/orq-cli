//! The sidebar's type, spacing, selected colour, single-cog Settings icon and sun/moon switch follow
//! the reference image, and its bottom items are never cut off.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

fn source(name: &str) -> String {
    std::fs::read_to_string(format!("{}/web-src/src/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn the_sidebar_sizes_its_box_with_the_padding_inside_so_the_bottom_items_are_not_pushed_out_of_view() {
    let css = css();
    assert!(rule(&css, ".sidebar {").contains("box-sizing: border-box"));
}

#[test]
fn settings_shows_a_single_cog_not_two() {
    let shell = source("shell.jsx");
    assert!(shell.contains("{ label: \"Settings\", icon: \"gear\""), "Settings uses the one-cog icon");
    assert!(!source("icons.jsx").contains("cogs:"), "the two-cog icon is gone");
}

#[test]
fn the_selected_item_is_a_light_blue_pill_with_dark_text_like_the_reference() {
    let css = css();
    assert_eq!(css.matches("--sidebar-active-bg: #dbeafe").count(), 4, "light blue in every token block");
    assert_eq!(css.matches("--sidebar-active-text: #0f172a").count(), 4, "dark text in every token block");
    let link = rule(&css, ".sidebar nav a {");
    assert!(link.contains("min-height: 38px") && link.contains("font-size: 0.9375rem") && link.contains("gap: 0.75rem"), "{link}");
    assert!(!css.contains(".sidebar nav a.section-link"), "the sections line up with Dashboard, not indented");
}

#[test]
fn the_sidebar_is_240px_wide_with_a_larger_brand_and_a_quiet_collapse_button() {
    let css = css();
    assert!(rule(&css, ".shell {").contains("grid-template-columns: 240px 1fr"));
    assert!(rule(&css, ".sidebar-head .brand {").contains("font-size: 1.375rem"));
    assert!(css.contains(".rail-button { background: transparent"));
}

#[test]
fn the_project_row_reads_as_plain_text_with_a_chevron_like_the_reference() {
    let css = css();
    let row = rule(&css, ".project-switch {");
    assert!(row.contains("font-size: 0.9375rem") && row.contains("font-weight: 600"), "{row}");
}

#[test]
fn the_theme_switch_is_one_button_holding_a_sun_and_a_moon_with_the_current_one_marked() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "responses": [], "steps": [{"nav": "[data-theme-toggle]"}], "selectors": ["[data-theme-toggle] .theme-option", "[data-theme-toggle] .theme-option[data-active]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    for frame in [0, 1] {
        let matches = &out["frames"][frame]["settled"]["matches"];
        assert_eq!(matches["[data-theme-toggle] .theme-option"], 3, "{out}");
        assert_eq!(matches["[data-theme-toggle] .theme-option[data-active]"], 1, "{out}");
    }
    assert_eq!(out["frames"][0]["settled"]["toggles"], 1, "{out}");
    let css = css();
    assert!(css.contains(".theme-toggle { display: inline-flex; align-items: center; justify-content: center; gap: 2px; padding: 3px; min-height: 0; min-width: 0; border-radius: 999px"));
}
