//! A run's logs: the files it wrote, then one file's redacted lines a page at a time, each page its
//! own address so nothing is loaded all at once.

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

#[test]
fn the_run_page_opens_its_log_files_listed_with_their_sizes() {
    let list = files(json!([{"name": "implement.log", "bytes": 2048}, {"name": "<b>x</b>.log", "bytes": 5}]));
    let Some(out) = run(RUN, json!([timeline(), projects(), list]), json!([{"submit": TOKEN}, {"click": "button[data-open=logs]", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], format!("{RUN}/logs"), "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/alpha/runs/run-1/logs", "{out}");
    assert_eq!(out["requests"][2]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let t = texts(&out["frames"][2]);
    assert!(t.iter().any(|x| x == "implement.log") && t.iter().any(|x| x == "2048 bytes") && t.iter().any(|x| x == "<b>x</b>.log"), "{t:?}");

    let Some(out) = run(&format!("{RUN}/logs"), json!([files(json!([])), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x == "No logs for this run."), "{out}");
}

#[test]
fn a_file_shows_its_lines_as_text_and_each_further_page_is_its_own_address() {
    let list = files(json!([{"name": "implement.log", "bytes": 9}]));
    let first = lines("implement.log", json!(["<i>one</i>", "two"]), json!("2"), false);
    let second = lines("implement.log", json!(["three"]), Value::Null, true);
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-file]", "index": 0}, {"click": "button[data-more=lines]", "index": 0}]);
    let Some(out) = run(&format!("{RUN}/logs"), json!([list, projects(), first, second]), steps) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], format!("{RUN}/logs/implement.log"), "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/alpha/runs/run-1/logs/implement.log", "{out}");
    let t = texts(&out["frames"][2]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("<i>one</i>") < at("two"), "{t:?}");
    assert!(!t.iter().any(|x| x.contains("cut short")), "{t:?}");

    assert_eq!(out["frames"][3]["settled"]["path"], format!("{RUN}/logs/implement.log/after/2"), "{out}");
    assert_eq!(out["requests"][3]["url"], "/api/projects/alpha/runs/run-1/logs/implement.log/after/2", "{out}");
    let t = texts(&out["frames"][3]);
    assert!(t.iter().any(|x| x == "three") && !t.iter().any(|x| x == "two"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("cut short")) && !t.iter().any(|x| x.contains("Load more")), "{t:?}");
}

#[test]
fn unusable_data_draws_no_lines_and_an_odd_address_is_not_found() {
    let bad = json!({"status": 200, "json": {"name": "implement.log", "lines": [5], "next_cursor": null, "truncated": false}});
    let Some(out) = run(&format!("{RUN}/logs/implement.log"), json!([bad, projects()]), json!([{"submit": TOKEN}])) else { return };
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
    let page = lines(names[0], json!(["ok"]), Value::Null, false);
    let Some(out) = run(&format!("{RUN}/logs"), json!([list, projects(), page]), json!([{"submit": TOKEN}, {"click": "button[data-file]", "index": 0}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter(|t| t.as_str() == Some("Show")).count(), 3, "{out}");
    assert_eq!(out["requests"][2]["url"], "/api/projects/alpha/runs/run-1/logs/my%20build%20%C3%A9.log", "{out}");
    assert!(texts(&out["frames"][2]).iter().any(|x| x == "ok"), "{out}");
    // A direct address with such a name is opened, not refused.
    let page = lines("a+b (1).log", json!(["direct"]), Value::Null, false);
    let Some(out) = run(&format!("{RUN}/logs/a%2Bb%20(1).log"), json!([page, projects()]), json!([{"submit": TOKEN}])) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x == "direct"), "{out}");
}
