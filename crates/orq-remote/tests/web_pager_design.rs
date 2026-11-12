//! One pager for every paged list: the page it is on, then Previous and Next side by side with their chevrons.
//! A log shows it only when the log has more than one page; its line count and Download stay either way.
//! Every button is drawn in the page's own font, not the browser's default.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";
const SELECTORS: [&str; 7] = [
    "nav.pager .pager-buttons button[data-page-prev] svg",
    "nav.pager .pager-buttons button[data-page-next] svg",
    "nav.pager .pager-buttons button[data-log-prev] svg",
    "nav.pager .pager-buttons button[data-more=lines] svg",
    "[data-log-prev]",
    "[data-more=lines]",
    "[data-log-download]",
];

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn timeline() -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": null, "started_at": null, "updated_at": null});
    json!({"status": 200, "json": {"run": header, "events": [], "next_cursor": null}})
}

fn logs(next: Value) -> Vec<Value> {
    let files = json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}]}});
    let page = json!({"status": 200, "json": {"name": "implement.log", "lines": ["one", "two"], "next_cursor": next, "truncated": false}});
    vec![timeline(), projects(), json!({"status": 200, "stream": [], "end": "hold"}), files, page]
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_log_of_one_page_shows_no_pager_but_keeps_its_count_and_download() {
    let Some(out) = run(RUN, json!(logs(Value::Null)), json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}])) else { return };
    let shown = last(&out);
    assert_eq!(count(shown, "[data-log-prev]"), 0, "{out}");
    assert_eq!(count(shown, "[data-more=lines]"), 0, "{out}");
    assert_eq!(count(shown, "[data-log-download]"), 1, "{out}");
    assert!(texts(shown).iter().any(|t| t == "2 of 2 lines"), "{out}");
    assert!(!texts(shown).iter().any(|t| t.starts_with("Page ")), "{out}");
}

#[test]
fn a_longer_log_and_a_long_feature_list_share_one_pager_with_chevrons() {
    let Some(out) = run(RUN, json!(logs(json!("2"))), json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}])) else { return };
    let shown = last(&out);
    assert_eq!(count(shown, SELECTORS[2]), 1, "{out}");
    assert_eq!(count(shown, SELECTORS[3]), 1, "{out}");
    assert!(texts(shown).iter().any(|t| t == "Previous"), "{out}");

    let feature = |n: u32| json!({"name": format!("f{n:02}"), "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    let many = json!({"status": 200, "json": {"features": (1..=12).map(feature).collect::<Vec<_>>()}});
    let Some(out) = run("/projects/alpha/features", json!([many, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(count(last(&out), SELECTORS[0]), 1, "{out}");
    assert_eq!(count(last(&out), SELECTORS[1]), 1, "{out}");
}

#[test]
fn every_button_takes_the_page_font() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let rule = css.lines().find(|line| line.starts_with("button {") && line.contains("background")).expect("the base button rule");
    assert!(rule.contains("font: inherit"), "{rule}");
}
