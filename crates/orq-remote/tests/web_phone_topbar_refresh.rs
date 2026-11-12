//! On a phone the top bar carries Refresh where Sign out was: every page can be refreshed from the bar,
//! so the page header has no Refresh of its own there. Signing out stays on the Settings page. A desktop
//! keeps Refresh in each page's header.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 4] = [".topbar > [data-refresh]", ".topbar > .sign-out", ".page-header [data-refresh]", "[data-refresh]"];

fn projects(active: u64) -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": active, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run(phone: bool, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "phone": phone, "responses": [projects(2), projects(5)], "steps": steps, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn a_phones_top_bar_refreshes_the_page_in_place_of_sign_out() {
    let Some(out) = run(true, json!([{"submit": TOKEN}, {"nav": ".topbar > [data-refresh]"}])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], ".topbar > [data-refresh]"), 1, "{out}");
    assert_eq!(count(&frames[1], ".topbar > .sign-out"), 0, "{out}");
    assert_eq!(count(&frames[1], "[data-refresh]"), 1, "only the bar's Refresh: {out}");
    let projects_asked = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/projects").count();
    assert_eq!(projects_asked, 2, "the bar's Refresh read the page again: {out}");
    let texts: Vec<&str> = frames[2]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"5"), "the fresh count is shown: {texts:?}");
}

#[test]
fn a_desktop_keeps_refresh_in_the_page_header() {
    let Some(out) = run(false, json!([{"submit": TOKEN}])) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(count(last, ".page-header [data-refresh]"), 1, "{out}");
    assert_eq!(count(last, "[data-refresh]"), 1, "{out}");
}
