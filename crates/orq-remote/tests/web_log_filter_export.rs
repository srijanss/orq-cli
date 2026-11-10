//! The log view filters the lines it shows by text and by level, and downloads exactly the lines
//! shown. Filtering and the download happen in the page: nothing about them is sent to the server.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const FILE: &str = "/projects/alpha/orchestrations/run-1/logs/implement.log";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": FILE, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project]}})
}

fn page() -> Value {
    let lines = json!(["INFO start", "ERROR boom <b>x</b>", "WARN slow", "plain text", "ERROR second boom"]);
    json!({"status": 200, "json": {"name": "implement.log", "lines": lines, "next_cursor": null, "truncated": false}})
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

fn open(steps: Value) -> Option<Value> {
    run(opened("implement.log", &[page()]), steps)
}

fn type_in(selector: &str, value: &str) -> Value {
    json!({"set": selector, "value": value})
}

fn shown(frame: &Value) -> Vec<String> {
    let lines = ["INFO start", "ERROR boom <b>x</b>", "WARN slow", "plain text", "ERROR second boom"];
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).filter(|t| lines.contains(t)).map(String::from).collect()
}

#[test]
fn typing_in_the_filter_keeps_the_lines_with_that_text_whatever_its_case_and_clearing_restores_them() {
    let steps = json!([{"submit": TOKEN}, type_in("[data-log-filter]", "BOOM"), type_in("[data-log-filter]", "")]);
    let Some(out) = open(steps) else { return };
    assert_eq!(shown(&out["frames"][1]).len(), 5, "{out}");
    assert_eq!(shown(&out["frames"][2]), vec!["ERROR boom <b>x</b>", "ERROR second boom"], "{out}");
    assert_eq!(shown(&out["frames"][3]).len(), 5, "{out}");
    let t: Vec<_> = out["frames"][2]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(t.iter().any(|x| x.contains("2 of 5 lines")), "{t:?}");
}

#[test]
fn the_level_picks_the_lines_of_that_level_and_combines_with_the_text() {
    let steps = json!([{"submit": TOKEN}, type_in("[data-log-level]", "error"), type_in("[data-log-filter]", "second"), type_in("[data-log-filter]", ""), type_in("[data-log-level]", "warn"), type_in("[data-log-level]", "all")]);
    let Some(out) = open(steps) else { return };
    assert_eq!(shown(&out["frames"][2]), vec!["ERROR boom <b>x</b>", "ERROR second boom"], "{out}");
    assert_eq!(shown(&out["frames"][3]), vec!["ERROR second boom"], "{out}");
    assert_eq!(shown(&out["frames"][4]).len(), 2, "{out}");
    assert_eq!(shown(&out["frames"][5]), vec!["WARN slow"], "{out}");
    assert_eq!(shown(&out["frames"][6]).len(), 5, "{out}");
}

#[test]
fn the_download_holds_exactly_the_shown_lines_as_plain_text_named_for_the_log() {
    let steps = json!([{"submit": TOKEN}, type_in("[data-log-level]", "error"), {"click": "button[data-log-download]"}]);
    let Some(out) = open(steps) else { return };
    let downloads = out["downloads"].as_array().unwrap();
    assert_eq!(downloads.len(), 1, "{out}");
    assert_eq!(downloads[0]["name"], "implement.log", "{out}");
    assert_eq!(downloads[0]["text"], "ERROR boom <b>x</b>\nERROR second boom\n", "{out}");
    assert!(downloads[0]["type"].as_str().unwrap().starts_with("text/plain"), "{out}");
    assert_eq!(downloads[0]["revoked"], true, "the temporary address is released: {out}");
}

#[test]
fn nothing_shown_means_nothing_to_download_and_nothing_is_sent_to_the_server() {
    let steps = json!([{"submit": TOKEN}, type_in("[data-log-filter]", "zzz-secret-filter"), {"click": "button[data-log-download]"}]);
    let Some(out) = open(steps) else { return };
    assert!(shown(&out["frames"][2]).is_empty(), "{out}");
    assert_eq!(out["downloads"].as_array().unwrap().len(), 0, "{out}");
    let urls: Vec<_> = out["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(urls.len(), 5, "{urls:?}");
    assert!(urls.iter().all(|u| !u.contains("zzz") && !u.contains('?')), "{urls:?}");
    assert!(out["frames"][2]["settled"]["texts"].as_array().unwrap().iter().any(|t| t.as_str().is_some_and(|t| t.contains("No lines match"))), "{out}");
}

#[test]
fn the_filters_stay_applied_when_the_next_page_of_lines_is_loaded() {
    let first = json!({"status": 200, "json": {"name": "implement.log", "lines": ["INFO start", "ERROR boom one"], "next_cursor": "2", "truncated": false}});
    let second = json!({"status": 200, "json": {"name": "implement.log", "lines": ["INFO later", "ERROR boom two", "ERROR other"], "next_cursor": null, "truncated": false}});
    let steps = json!([{"submit": TOKEN}, type_in("[data-log-level]", "error"), type_in("[data-log-filter]", "boom"), {"click": "button[data-more=lines]"}]);
    let files = json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}]}});
    let Some(out) = run(json!([run_page(), projects(), {"status": 200, "stream": [], "end": "hold"}, files, first, second]), steps) else { return };
    let texts = |i: usize| -> Vec<String> { out["frames"][i]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect() };
    // The next page comes from the cursor without leaving the run's page.
    assert!(out["requests"].as_array().unwrap().iter().any(|r| r["url"].as_str().unwrap_or("").ends_with("/logs/implement.log/after/2")), "{out}");
    assert!(out["frames"][4]["settled"]["path"].as_str().unwrap().ends_with("/logs/implement.log"), "{out}");
    let t = texts(4);
    assert!(t.iter().any(|x| x == "ERROR boom two") && !t.iter().any(|x| x == "INFO later" || x == "ERROR other"), "{t:?}");
    let inputs = out["frames"][4]["settled"]["inputs"].as_array().unwrap();
    assert!(inputs.iter().any(|i| i["value"] == "boom"), "{inputs:?}");
}
