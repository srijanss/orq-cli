//! The open list or report re-reads about every 30 seconds, only while the tab is visible, never
//! overlapping itself, and says when it last checked.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const EVERY: u64 = 30_000;

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

fn list(names: &[&str]) -> Value {
    let rows: Vec<Value> = names.iter().map(|n| json!({"run_id": format!("run-{n}"), "feature": n, "project": "p", "current": true, "status": "approved", "risk": "low", "stale": false, "link": null, "generated_at": null})).collect();
    json!({"status": 200, "json": {"reviews": rows}})
}

fn texts(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn a_loaded_list_asks_again_every_thirty_seconds_with_the_same_token_and_shows_the_new_rows() {
    let Some(out) = run("/", json!([list(&["first"]), list(&["second"])]), json!([{"submit": TOKEN}, {"tick": EVERY}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["activeIntervals"], json!([EVERY]), "{out}");
    let requests = out["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "{out}");
    assert_eq!(requests[1]["url"], requests[0]["url"]);
    assert_eq!(requests[1]["headers"], requests[0]["headers"]);
    let frame = &out["frames"][2];
    assert!(texts(frame).contains("second") && !texts(frame).contains("first"), "{out}");
    assert_eq!(frame["settled"]["inputs"].as_array().unwrap().len(), 0, "still signed in: {out}");
}

#[test]
fn an_open_report_is_re_read_too() {
    let detail = json!({"status": 200, "json": {"run_id": "run-1", "reported_head_sha": "abc1234", "observed_head_sha": "abc1234", "stale": false, "link": null, "summary": ["one"], "risks": [], "files": [], "tests": [], "rounds": [], "generated_at": null}});
    let Some(out) = run("/", json!([list(&["first"]), detail.clone(), detail]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"tick": EVERY}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 3, "{out}");
    assert_eq!(out["requests"][2]["url"], out["requests"][1]["url"], "{out}");
}

#[test]
fn a_hidden_tab_does_not_ask_and_coming_back_asks_at_once() {
    let Some(out) = run("/", json!([list(&["first"]), list(&["second"])]), json!([{"submit": TOKEN}, {"visibility": "hidden"}, {"tick": EVERY}, {"visibility": "visible"}])) else { return };
    assert_eq!(out["frames"][3]["settled"]["activeIntervals"], json!([EVERY]), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().len(), 2, "hidden tick must not ask, return must: {out}");
    assert!(texts(&out["frames"][4]).contains("second"), "{out}");
    let hidden_only = run("/", json!([list(&["first"]), list(&["second"])]), json!([{"submit": TOKEN}, {"visibility": "hidden"}, {"tick": EVERY}])).unwrap();
    assert_eq!(hidden_only["requests"].as_array().unwrap().len(), 1, "{hidden_only}");
}

#[test]
fn a_refresh_never_starts_while_the_last_one_is_still_waiting() {
    let held = json!({"hold": "fetch", "status": 200, "json": {"reviews": []}});
    let Some(out) = run("/", json!([list(&["first"]), held, list(&["third"])]), json!([{"submit": TOKEN}, {"tick": EVERY}, {"tick": EVERY}, {"release": 1}, {"tick": EVERY}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 3, "the second tick while waiting must not ask, the one after the answer must: {out}");
}

#[test]
fn the_page_says_when_it_last_checked_and_stops_asking_when_left() {
    let Some(out) = run("/", json!([list(&["first"])]), json!([{"submit": TOKEN}, {"go": "/settings"}])) else { return };
    let checked = out["frames"][1]["settled"]["checked"].as_array().unwrap().clone();
    assert_eq!(checked.len(), 1, "{out}");
    assert!(checked[0].as_str().unwrap().starts_with("Last checked "), "{out}");
    assert_eq!(out["frames"][2]["settled"]["activeIntervals"], json!([]), "{out}");
}

#[test]
fn a_refused_token_on_a_refresh_asks_for_the_token_again_and_a_failed_one_keeps_the_rows() {
    let Some(out) = run("/", json!([list(&["first"]), {"status": 401}]), json!([{"submit": TOKEN}, {"tick": EVERY}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
    let Some(out) = run("/", json!([list(&["first"]), {"reject": true}]), json!([{"submit": TOKEN}, {"tick": EVERY}])) else { return };
    assert!(texts(&out["frames"][2]).contains("first"), "{out}");
    assert_eq!(out["frames"][2]["settled"]["inputs"].as_array().unwrap().len(), 0, "{out}");
}

#[test]
fn leaving_during_a_waiting_refresh_cancels_it_and_nothing_asks_again_afterwards() {
    let held = json!({"hold": "fetch", "status": 200, "json": {"reviews": []}});
    let steps = json!([{"submit": TOKEN}, {"tick": EVERY}, {"go": "/settings"}, {"release": 1}, {"visibility": "hidden"}, {"visibility": "visible"}, {"tick": EVERY}]);
    let Some(out) = run("/", json!([list(&["first"]), held, list(&["late"])]), steps) else { return };
    let requests = out["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "no request after leaving: {out}");
    assert_eq!(requests[1]["aborted"], true, "{out}");
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(last["settled"]["path"], "/settings", "{out}");
    assert_eq!(last["settled"]["activeIntervals"], json!([]), "{out}");
    assert_eq!(out["frames"][1]["settled"]["visibilityListeners"], 1, "the page listens while it is open: {out}");
    assert_eq!(last["settled"]["visibilityListeners"], 0, "leaving removes the listener: {out}");
    assert!(!texts(last).contains("late") && !texts(last).contains("first"), "{out}");
}
