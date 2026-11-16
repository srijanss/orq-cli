//! A run a resume took over is `resumed`: it offers no action in the runs list or on its page, its pill
//! is a muted "Resumed", and its page links to the run that continues it, which links back. A page left
//! open on a run that gets resumed loses its actions on its next read.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const ACTIONS: [&str; 6] = ["[data-resume]", "[data-discard]", "[data-accept]", "[data-pause]", "[data-stop]", "[data-run-more]"];
const EVERY: u64 = 30_000;

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn one(id: &str, state: &str, links: Value) -> Value {
    let mut row = json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    for (key, value) in links.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

fn detail(run: Value, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let id = run["run_id"].as_str().unwrap().to_string();
    let body = json!({"status": 200, "json": {"run": run, "events": [], "next_cursor": null, "remote_actions": true}});
    let mut responses = vec![body, projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run_page(&format!("/projects/alpha/orchestrations/{id}"), responses, all)
}

fn run_page(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run(json!({"url": url, "responses": responses, "steps": steps, "selectors": ACTIONS, "token": TOKEN}))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn actions(frame: &Value) -> u64 {
    ACTIONS.iter().map(|sel| frame["settled"]["matches"][sel].as_u64().unwrap()).sum()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn links(frame: &Value) -> Vec<(String, String)> {
    frame["settled"]["links"].as_array().unwrap().iter().map(|l| (l["href"].as_str().unwrap_or("").to_string(), l["text"].as_str().unwrap_or("").to_string())).collect()
}

#[test]
fn a_resumed_run_has_no_actions_in_the_list_and_says_what_continued_it() {
    let rows = vec![one("run-1", "resumed", json!({"resumed_as": "run-2"})), one("run-2", "failed", json!({"resumed_from": "run-1"}))];
    let list = json!({"status": 200, "json": {"runs": rows, "next_cursor": null, "total": 2, "running": 0, "remote_actions": true}});
    let Some(out) = run_page("/projects/alpha/orchestrations", vec![list, projects()], vec![json!({"submit": TOKEN})]) else { return };
    let frame = last(&out);
    assert_eq!(frame["settled"]["matches"]["[data-resume]"], json!(1), "only run-2 resumes: {out}");
    assert_eq!(frame["settled"]["matches"]["[data-run-more]"], json!(1), "only run-2 has more actions: {out}");
    let shown = texts(frame);
    assert!(shown.iter().any(|t| t == "Resumed"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "continued as run-2"), "{shown:?}");
    assert!(frame["settled"]["classes"].as_array().unwrap().contains(&json!("run-resumed")), "{out}");
}

#[test]
fn a_resumed_runs_page_has_no_actions_and_links_to_the_run_that_continues_it() {
    let Some(out) = detail(one("run-1", "resumed", json!({"resumed_as": "run-2"})), vec![], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(actions(frame), 0, "{out}");
    assert!(texts(frame).iter().any(|t| t == "Resumed"), "{out}");
    assert!(links(frame).contains(&("/projects/alpha/orchestrations/run-2".into(), "run-2".into())), "{out}");
    assert!(texts(frame).iter().any(|t| t.starts_with("Continued as")), "{out}");
}

#[test]
fn the_run_that_continues_one_links_back_to_it() {
    let Some(out) = detail(one("run-2", "running", json!({"resumed_from": "run-1"})), vec![], vec![]) else { return };
    let frame = last(&out);
    assert!(links(frame).contains(&("/projects/alpha/orchestrations/run-1".into(), "run-1".into())), "{out}");
    assert!(texts(frame).iter().any(|t| t.starts_with("Resumed from")), "{out}");
}

#[test]
fn a_hostile_resume_link_is_text_and_never_a_link_or_markup() {
    let hostile = "<img src=x onerror=alert(1)>";
    let Some(out) = detail(one("run-1", "resumed", json!({"resumed_as": hostile})), vec![], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(frame["settled"]["injected"], json!(0), "{out}");
    assert!(links(frame).iter().all(|(href, _)| !href.contains("img")), "{out}");
    assert!(texts(frame).iter().any(|t| t.contains(hostile)), "shown as plain text: {out}");
}

#[test]
fn a_page_left_open_on_a_run_that_gets_resumed_loses_its_actions_on_its_next_read() {
    let resumed = json!({"status": 200, "json": {"run": one("run-1", "resumed", json!({"resumed_as": "run-2"})), "events": [], "next_cursor": null, "remote_actions": true}});
    let Some(out) = detail(one("run-1", "failed", json!({})), vec![resumed, projects()], vec![json!({"tick": EVERY})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(actions(&frames[1]) > 0, "a failed run has actions first: {out}");
    let frame = last(&out);
    assert_eq!(actions(frame), 0, "{out}");
    assert!(links(frame).contains(&("/projects/alpha/orchestrations/run-2".into(), "run-2".into())), "{out}");
}
