//! The Dashboard items follow the reference: each total is a card with a tinted icon beside a large
//! value and its label, and each project card shows its figures with small icons.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

#[test]
fn every_total_has_an_icon_chip_and_every_project_figure_a_small_icon() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 2, "reviews_waiting": 3, "failed_runs": 0, "failed_window_hours": 24});
    let sel = json!([".stats .stat > .stat-icon svg", ".stats .stat > .stat-text > .stat-value", ".project .card-stat > svg", ".connection"]);
    let scenario = json!({"url": "/dashboard", "responses": [{"status": 200, "json": {"projects": [project]}}], "steps": [{"submit": TOKEN}], "selectors": sel});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let m = &out["frames"][1]["settled"]["matches"];
    assert_eq!(m[".stats .stat > .stat-icon svg"], 4, "{out}");
    assert_eq!(m[".stats .stat > .stat-text > .stat-value"], 4, "{out}");
    assert_eq!(m[".project .card-stat > svg"], 3, "{out}");
    assert_eq!(m[".connection"], 1, "{out}");
}

#[test]
fn the_total_is_a_row_with_a_tinted_round_cornered_chip_and_the_clock_icon_exists() {
    let css = css();
    let stat = rule(&css, "\n.stat {");
    assert!(stat.contains("flex-direction: row") && stat.contains("align-items: center"), "{stat}");
    let chip = rule(&css, ".stat-icon {");
    assert!(chip.contains("width: 3rem") && chip.contains("border-radius: 0.75rem") && chip.contains("color-mix("), "{chip}");
    assert!(css.contains(".header-status {"), "the status sits at the right of the header row");
    let icons = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/icons.jsx")).unwrap();
    assert!(icons.contains("  clock: \""), "the clock icon is drawn");
}
