//! The Dashboard's first row: title and subtext on the left; the connection, the last-updated time
//! and the refresh button together on the right of the same row.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

#[test]
fn title_subtext_connection_last_updated_and_refresh_share_one_header_row() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 2, "reviews_waiting": 3, "failed_runs": 0, "failed_window_hours": 24});
    let sel = json!([".page-header > div > h1", ".page-header > div > .subtitle", ".page-header > .header-status > .connection", ".page-header > .header-status > .last-updated time", ".page-header > .header-status > [data-refresh]", ".connection", "[data-refresh]"]);
    let scenario = json!({"url": "/dashboard", "responses": [{"status": 200, "json": {"projects": [project]}}], "steps": [{"submit": TOKEN}], "selectors": sel});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let settled = &out["frames"][1]["settled"];
    let m = &settled["matches"];
    for k in [".page-header > div > h1", ".page-header > div > .subtitle", ".page-header > .header-status > .connection", ".page-header > .header-status > .last-updated time", ".page-header > .header-status > [data-refresh]", ".connection", "[data-refresh]"] {
        assert_eq!(m[k], 1, "{k}: {out}");
    }
    let texts: Vec<&str> = settled["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Connected (Tailscale)"), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("Last updated:")), "{texts:?}");
}

#[test]
fn the_status_group_sits_at_the_right_of_the_row() {
    let css = css();
    let at = css.find(".header-status {").expect("no .header-status rule");
    let rule = &css[at..at + css[at..].find('}').unwrap()];
    assert!(rule.contains("display: flex") && rule.contains("align-items: center") && rule.contains("margin-left: auto"), "{rule}");
}
