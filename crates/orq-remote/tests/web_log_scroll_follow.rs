//! A log page that gains lines follows them only while the viewer is at the bottom. Scrolling up
//! pauses it and shows how many new lines are waiting, with a Jump to latest control. The test
//! scroller is ten pixels a line in a box 30 high, with the scroll offset kept in range as a
//! browser does: ten lines can scroll to 70, thirteen to 100.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const LOG: &str = "/projects/alpha/orchestrations/run-1/logs/stage.log";
const EVERY: u64 = 30000;
const BOX: &str = ".log-lines";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": LOG, "responses": responses, "steps": steps, "scroller": {"client": 30}});
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

fn run_page() -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": null, "started_at": null, "updated_at": null});
    json!({"status": 200, "json": {"run": header, "events": [], "next_cursor": null}})
}

// A log address opens the run's page on its Logs tab: the run, the projects, its event stream and
// its file list come first, then the page. Every refresh after that asks for the page, the list and
// the run again, in that order.
fn opened(name: &str, pages: &[Value]) -> Value {
    let files = json!({"status": 200, "json": {"files": [{"name": name, "bytes": 9}]}});
    let mut all = vec![run_page(), projects(), json!({"status": 200, "stream": [], "end": "hold"}), files.clone(), pages[0].clone()];
    for page in &pages[1..] {
        all.extend([page.clone(), files.clone(), run_page()]);
    }
    Value::Array(all)
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

fn says(frame: &Value, text: &str) -> bool {
    texts(frame).iter().any(|t| t == text)
}

fn waiting(frame: &Value) -> bool {
    texts(frame).iter().any(|t| t.contains("new line"))
}

#[test]
fn a_viewer_at_the_bottom_follows_new_lines() {
    let Some(out) = run(opened("stage.log", &[page(10), page(13)]), json!([{"submit": TOKEN}, {"scrollto": BOX, "top": 70}, {"tick": EVERY}])) else { return };
    let frame = &out["frames"][3];
    assert_eq!(top(frame), 100, "the new bottom: {out}");
    assert!(!waiting(frame), "{:?}", texts(frame));
    assert!(!buttons(frame).iter().any(|b| b == "Jump to latest"), "{:?}", buttons(frame));
}

#[test]
fn scrolling_up_pauses_following_and_counts_the_new_lines() {
    let Some(out) = run(opened("stage.log", &[page(10), page(13), page(14)]), json!([{"submit": TOKEN}, {"scrollto": BOX, "top": 70}, {"scrollto": BOX, "top": 0}, {"tick": EVERY}, {"tick": EVERY}])) else { return };
    let frame = &out["frames"][4];
    assert_eq!(top(frame), 0, "the viewer's place is kept: {out}");
    assert!(says(frame, "3 new lines"), "{:?}", texts(frame));
    assert!(buttons(frame).iter().any(|b| b == "Jump to latest"), "{:?}", buttons(frame));
    let last = &out["frames"][5];
    assert!(says(last, "4 new lines"), "{:?}", texts(last));
    assert_eq!(top(last), 0, "{out}");
}

#[test]
fn a_log_opened_at_the_top_is_not_following_until_the_viewer_reaches_the_bottom() {
    let Some(out) = run(opened("stage.log", &[page(10), page(12)]), json!([{"submit": TOKEN}, {"tick": EVERY}])) else { return };
    let frame = &out["frames"][2];
    assert_eq!(top(frame), 0, "it was never scrolled, so it is not moved: {out}");
    assert!(says(frame, "2 new lines"), "{:?}", texts(frame));
}

#[test]
fn one_new_line_is_singular() {
    let Some(out) = run(opened("stage.log", &[page(10), page(11)]), json!([{"submit": TOKEN}, {"tick": EVERY}])) else { return };
    assert!(says(&out["frames"][2], "1 new line"), "{:?}", texts(&out["frames"][2]));
}

#[test]
fn jump_to_latest_scrolls_to_the_bottom_clears_the_count_and_follows_again() {
    let Some(out) = run(opened("stage.log", &[page(10), page(13), page(15)]), json!([{"submit": TOKEN}, {"tick": EVERY}, {"click": "[data-jump-latest]"}, {"tick": EVERY}])) else { return };
    let jumped = &out["frames"][3];
    assert_eq!(top(jumped), 100, "{out}");
    assert!(!waiting(jumped), "{:?}", texts(jumped));
    assert_eq!(top(&out["frames"][4]), 120, "following again: {out}");
}

#[test]
fn scrolling_back_to_the_bottom_by_hand_clears_the_count_and_resumes_following() {
    let Some(out) = run(opened("stage.log", &[page(10), page(13), page(15)]), json!([{"submit": TOKEN}, {"tick": EVERY}, {"scrollto": BOX, "top": 100}, {"tick": EVERY}])) else { return };
    assert!(!waiting(&out["frames"][3]), "{:?}", texts(&out["frames"][3]));
    assert_eq!(top(&out["frames"][4]), 120, "{out}");
}

#[test]
fn filtering_a_paused_log_down_to_a_few_lines_and_back_keeps_it_paused() {
    let Some(out) = run(opened("stage.log", &[page(10), page(12)]), json!([{"submit": TOKEN}, {"scrollto": BOX, "top": 0}, {"set": "[data-log-filter]", "value": "line 1"}, {"set": "[data-log-filter]", "value": ""}, {"tick": EVERY}])) else { return };
    for frame in &out["frames"].as_array().unwrap()[2..5] {
        assert!(!waiting(frame), "filtering is not new lines: {:?}", texts(frame));
    }
    let last = &out["frames"][5];
    assert_eq!(top(last), 0, "{out}");
    assert!(says(last, "2 new lines"), "{:?}", texts(last));
}

#[test]
fn filtering_a_paused_log_to_a_shorter_but_still_scrollable_result_keeps_it_paused() {
    // Paused at 50 of 15 lines; "line 1" keeps six lines, whose largest offset is 30, so the browser
    // moves the offset there and says so with a scroll event while the result still overflows.
    let Some(out) = run(opened("stage.log", &[page(15), page(17)]), json!([{"submit": TOKEN}, {"scrollto": BOX, "top": 50}, {"set": "[data-log-filter]", "value": "line 1"}, {"set": "[data-log-filter]", "value": ""}, {"tick": EVERY}])) else { return };
    let last = &out["frames"][5];
    assert!(says(last, "2 new lines"), "still paused, not following: {:?}", texts(last));
    assert!(top(last) < 140, "it did not jump to the new bottom: {out}");
}
