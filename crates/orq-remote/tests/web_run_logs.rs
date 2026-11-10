//! A run's logs sit under the run page's Logs tab: the files it wrote, then one file's redacted lines
//! a page at a time, without leaving the run's page. The Timeline tab shows the run's steps.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";

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

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn files(list: Value) -> Value {
    json!({"status": 200, "json": {"files": list}})
}

fn lines(name: &str, lines: Value, next: Value, truncated: bool) -> Value {
    json!({"status": 200, "json": {"name": name, "lines": lines, "next_cursor": next, "truncated": truncated}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn timeline() -> Value {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": null, "started_at": null, "updated_at": null});
    json!({"status": 200, "json": {"run": header, "events": [], "next_cursor": null}})
}

// The run page follows its events, so its stream is the third request; the logs come after it.
fn stream() -> Value {
    json!({"status": 200, "stream": [], "end": "hold"})
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn the_logs_tab_lists_the_files_with_their_sizes_and_opens_the_first_without_leaving_the_run_page() {
    let list = files(json!([{"name": "implement.log", "bytes": 2048}, {"name": "<b>x<b>.log", "bytes": 5}]));
    let page = lines("implement.log", json!(["10:00:01 INFO start"]), Value::Null, false);
    let Some(out) = run(RUN, json!([timeline(), projects(), stream(), list, page]), json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], RUN, "{out}");
    assert_eq!(out["requests"][3]["url"], "/api/projects/alpha/runs/run-1/logs", "{out}");
    assert_eq!(out["requests"][3]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(out["requests"][4]["url"], "/api/projects/alpha/runs/run-1/logs/implement.log", "{out}");
    let t = texts(&out["frames"][2]);
    for want in ["implement.log · 2048 bytes", "<b>x<b>.log · 5 bytes", "Run run-1 · Logs", "Follow", "10:00:01", "Download"] {
        assert!(t.iter().any(|x| x == want), "{want} missing: {t:?}");
    }

    let Some(out) = run(RUN, json!([timeline(), projects(), stream(), files(json!([]))]), json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}])) else { return };
    assert!(texts(&out["frames"][2]).iter().any(|x| x == "No logs for this run."), "{out}");
}

#[test]
fn the_logs_load_once_and_switching_tabs_keeps_the_address_and_the_stream() {
    let list = files(json!([{"name": "implement.log", "bytes": 9}]));
    let page = lines("implement.log", json!(["one"]), Value::Null, false);
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}, {"click": "button[data-run-tab=timeline]"}, {"click": "button[data-run-tab=logs]"}, {"click": "button[data-run-tab=events]"}]);
    let Some(out) = run(RUN, json!([timeline(), projects(), stream(), list, page]), steps) else { return };
    for i in 1..=5 {
        assert_eq!(out["frames"][i]["settled"]["path"], RUN, "frame {i}: {out}");
    }
    assert_eq!(urls(&out).len(), 5, "nothing is fetched again: {:?}", urls(&out));
    assert!(texts(&out["frames"][5]).iter().any(|x| x == "No events recorded." || x == "one"), "{out}");
}

#[test]
fn a_file_shows_its_lines_as_text_and_next_loads_the_following_page_in_the_tab() {
    let list = files(json!([{"name": "implement.log", "bytes": 9}]));
    let first = lines("implement.log", json!(["<i>one</i>", "two"]), json!("2"), false);
    let second = lines("implement.log", json!(["three"]), Value::Null, true);
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}, {"click": "button[data-more=lines]"}, {"click": "button[data-log-prev]"}]);
    let Some(out) = run(RUN, json!([timeline(), projects(), stream(), list, first, second, first]), steps) else { return };
    let t = texts(&out["frames"][2]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("<i>one</i>") < at("two"), "{t:?}");
    assert!(t.iter().any(|x| x == "Page 1") && !t.iter().any(|x| x.contains("cut short")), "{t:?}");

    assert_eq!(out["frames"][3]["settled"]["path"], RUN, "{out}");
    assert_eq!(out["requests"][5]["url"], "/api/projects/alpha/runs/run-1/logs/implement.log/after/2", "{out}");
    let t = texts(&out["frames"][3]);
    assert!(t.iter().any(|x| x == "three") && !t.iter().any(|x| x == "two") && t.iter().any(|x| x == "Page 2"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("cut short")), "{t:?}");

    assert_eq!(out["requests"][6]["url"], "/api/projects/alpha/runs/run-1/logs/implement.log", "{out}");
    let t = texts(&out["frames"][4]);
    assert!(t.iter().any(|x| x == "two") && t.iter().any(|x| x == "Page 1"), "Prev goes back: {t:?}");
}

#[test]
fn an_old_log_address_opens_the_run_page_on_its_logs_tab() {
    let list = files(json!([{"name": "build.log", "bytes": 3}, {"name": "implement.log", "bytes": 9}]));
    let page = lines("implement.log", json!(["picked"]), json!("4"), false);
    let Some(out) = run(&format!("{RUN}/logs/implement.log/after/2"), json!([timeline(), projects(), stream(), list, page]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(urls(&out)[4], "/api/projects/alpha/runs/run-1/logs/implement.log/after/2", "{out}");
    let t = texts(&out["frames"][1]);
    assert!(t.iter().any(|x| x == "picked") && t.iter().any(|x| x == "Run run-1 · Logs"), "{t:?}");
}

#[test]
fn unusable_data_draws_no_lines_and_an_odd_address_is_not_found() {
    let bad = json!({"status": 200, "json": {"name": "implement.log", "lines": [5], "next_cursor": null, "truncated": false}});
    let list = files(json!([{"name": "implement.log", "bytes": 9}]));
    let Some(out) = run(&format!("{RUN}/logs/implement.log"), json!([timeline(), projects(), stream(), list, bad]), json!([{"submit": TOKEN}])) else { return };
    assert!(!texts(&out["frames"][1]).iter().any(|x| x == "5"), "{out}");
    for odd in ["x.txt", "a%2Fb.log", ".hidden.log", "implement.log/after/abc"] {
        let Some(out) = run(&format!("{RUN}/logs/{odd}"), json!([projects()]), json!([{"submit": TOKEN}])) else { return };
        assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"].as_str().unwrap_or("").contains("/runs")).count(), 0, "{odd}: {out}");
    }
}

#[test]
fn every_file_name_the_server_lists_can_be_opened() {
    let names = ["my build é.log", "a+b (1).log", &format!("{}.log", "n".repeat(200))];
    let list = files(json!(names.iter().map(|n| json!({"name": n, "bytes": 1})).collect::<Vec<_>>()));
    let page = |n: &str, line: &str| lines(n, json!([line]), Value::Null, false);
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-run-tab=logs]"}, {"set": "select[data-log-file]", "value": names[1]}]);
    let Some(out) = run(RUN, json!([timeline(), projects(), stream(), list, page(names[0], "ok"), page(names[1], "plus")]), steps) else { return };
    assert_eq!(urls(&out)[4], "/api/projects/alpha/runs/run-1/logs/my%20build%20%C3%A9.log", "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|x| x == "ok"), "{out}");
    assert_eq!(urls(&out)[5], "/api/projects/alpha/runs/run-1/logs/a%2Bb%20(1).log", "{out}");
    assert!(texts(&out["frames"][3]).iter().any(|x| x == "plus"), "{out}");
    // A direct address with such a name is opened, not refused.
    let list = files(json!([{"name": "a+b (1).log", "bytes": 1}]));
    let Some(out) = run(&format!("{RUN}/logs/a%2Bb%20(1).log"), json!([timeline(), projects(), stream(), list, page("a+b (1).log", "direct")]), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x == "direct"), "{out}");
}

#[test]
fn the_timeline_tab_lists_the_steps_done_and_the_one_in_progress_with_their_times() {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": "test", "started_at": null, "updated_at": null});
    let ev = |id: &str, kind: &str, stage: &str, at: &str| json!({"event_id": id, "kind": kind, "stage": stage, "attempt": 1, "occurred_at": at});
    let events = json!([ev("e0", "stage.started", "implement", "2026-11-02T10:00:00Z"), ev("e1", "stage.completed", "implement", "2026-11-02T10:01:05Z"), ev("e2", "stage.started", "test", "2026-11-02T10:01:10Z")]);
    let timeline = json!({"status": 200, "json": {"run": header, "events": events, "next_cursor": null}});
    let Some(out) = run(RUN, json!([timeline, projects(), stream()]), json!([{"submit": TOKEN}])) else { return };
    let t = texts(&out["frames"][1]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("implement") < at("test"), "{t:?}");
    assert!(t.iter().any(|x| x == "1m 5s") && t.iter().any(|x| x == "Done"), "{t:?}");
    assert!(t.iter().any(|x| x.starts_with("In progress")), "{t:?}");
}
