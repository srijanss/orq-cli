//! A log page that gains lines follows them only while the viewer is at the bottom. Scrolling up
//! pauses it and shows how many new lines are waiting, with a Jump to latest control.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const LOG: &str = "/projects/alpha/orchestrations/run-1/logs/stage.log";
const EVERY: u64 = 30000;

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": LOG, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn page(count: usize) -> Value {
    let lines: Vec<String> = (0..count).map(|n| format!("line {n}")).collect();
    json!({"status": 200, "json": {"name": "stage.log", "lines": lines, "next_cursor": null, "truncated": false}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn buttons(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn top(frame: &Value) -> i64 {
    frame["settled"]["logTop"].as_i64().unwrap()
}

// Ten pixels a line in a box 30 high: ten lines have their bottom at a top of 70.
const BOX: &str = ".log-lines";

#[test]
fn a_viewer_at_the_bottom_follows_new_lines() {
    let Some(out) = run(json!([page(10), projects(), page(13)]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"tick": EVERY}])) else { return };
    let frame = &out["frames"][3];
    assert_eq!(top(frame), 130, "the new bottom: {out}");
    assert!(!texts(frame).iter().any(|t| t.contains("new line")), "{:?}", texts(frame));
    assert!(!buttons(frame).iter().any(|b| b == "Jump to latest"), "{:?}", buttons(frame));
}

#[test]
fn scrolling_up_pauses_following_and_counts_the_new_lines() {
    let Some(out) = run(json!([page(10), projects(), page(13), page(14)]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"scrollto": BOX, "top": 0}, {"tick": EVERY}, {"tick": EVERY}])) else { return };
    let frame = &out["frames"][4];
    assert_eq!(top(frame), 0, "the viewer's place is kept: {out}");
    assert!(texts(frame).iter().any(|t| t == "3 new lines"), "{:?}", texts(frame));
    assert!(buttons(&out["frames"][4]).iter().any(|b| b == "Jump to latest"), "{:?}", buttons(&out["frames"][4]));
    let last = &out["frames"][5];
    assert!(texts(last).iter().any(|t| t == "4 new lines"), "{:?}", texts(last));
    assert_eq!(top(last), 0, "{out}");
}

#[test]
fn one_new_line_is_singular() {
    let Some(out) = run(json!([page(10), projects(), page(11)]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"scrollto": BOX, "top": 0}, {"tick": EVERY}])) else { return };
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "1 new line"), "{:?}", texts(&out["frames"][4]));
}

#[test]
fn jump_to_latest_scrolls_to_the_bottom_clears_the_count_and_follows_again() {
    let Some(out) = run(json!([page(10), projects(), page(13), page(15)]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"scrollto": BOX, "top": 0}, {"tick": EVERY}, {"click": "[data-jump-latest]"}, {"tick": EVERY}])) else { return };
    let jumped = &out["frames"][5];
    assert_eq!(top(jumped), 130, "{out}");
    assert!(!texts(jumped).iter().any(|t| t.contains("new line")), "{:?}", texts(jumped));
    assert_eq!(top(&out["frames"][6]), 150, "following again: {out}");
}

#[test]
fn scrolling_back_to_the_bottom_by_hand_clears_the_count_and_resumes_following() {
    let Some(out) = run(json!([page(10), projects(), page(13), page(15)]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"scrollto": BOX, "top": 0}, {"tick": EVERY}, {"scrollto": BOX, "top": 100}, {"tick": EVERY}])) else { return };
    assert!(!texts(&out["frames"][5]).iter().any(|t| t.contains("new line")), "{:?}", texts(&out["frames"][5]));
    assert_eq!(top(&out["frames"][6]), 150, "{out}");
}

#[test]
fn filtering_lines_is_not_counted_as_new_lines() {
    let Some(out) = run(json!([page(10), projects()]), json!([{"submit": TOKEN}, {"geometry": BOX, "client": 30, "top": 70}, {"scrollto": BOX, "top": 0}, {"set": "[data-log-filter]", "value": "line 1"}, {"set": "[data-log-filter]", "value": ""}])) else { return };
    for frame in &out["frames"].as_array().unwrap()[3..] {
        assert!(!texts(frame).iter().any(|t| t.contains("new line")), "{:?}", texts(frame));
    }
}
