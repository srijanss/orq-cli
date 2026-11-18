//! A Timeline step whose sessions changed files outside their features' files shows one closed line,
//! "Changed outside its features' files: N files", instead of one line per file. Opening it lists each
//! file once, in the order recorded; closing hides them again; and it stays open while new events arrive.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const TOGGLE: &str = "[data-step-undeclared-toggle]";
const OPENED: &str = "[data-step-undeclared-toggle][aria-expanded=\"true\"]";
const CLOSED: &str = "[data-step-undeclared-toggle][aria-expanded=\"false\"]";
const FILES: &str = "[data-step-undeclared]";

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn page(events: Value) -> Value {
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "completed", "stage": "implement_and_review", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}})
}

fn open_run(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": responses, "steps": all, "selectors": [TOGGLE, OPENED, CLOSED, FILES, ".run-step [data-step-undeclared-toggle]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn event(id: &str, kind: &str, files: Option<Value>) -> Value {
    let mut event = json!({"event_id": id, "kind": kind, "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-16T15:35:08Z"});
    if let Some(files) = files {
        event["undeclared_files"] = files;
    }
    event
}

/// An implement step whose two sessions named src/a.rs, src/b.rs and src/a.rs again.
fn three_named() -> Vec<Value> {
    vec![
        event("e0", "stage.started", None),
        event("e1", "session.undeclared_files", Some(json!(["src/a.rs", "src/b.rs"]))),
        event("e2", "session.undeclared_files", Some(json!(["src/a.rs"]))),
        event("e3", "stage.completed", None),
    ]
}

fn frame(out: &Value, at: usize) -> &Value {
    &out["frames"][at]
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_step_shows_one_closed_line_counting_its_files_once_each() {
    let Some(out) = open_run(vec![page(Value::Array(three_named())), projects()], vec![]) else { return };
    let shown = frame(&out, 1);
    assert_eq!((count(shown, ".run-step [data-step-undeclared-toggle]"), count(shown, CLOSED)), (1, 1), "{out}");
    assert_eq!(count(shown, FILES), 0, "no file is listed until it is opened: {out}");
    assert!(texts(shown).iter().any(|t| t == "Changed outside its features' files: 2 files"), "{:?}", texts(shown));
    assert!(!texts(shown).iter().any(|t| t.contains("src/a.rs")), "{:?}", texts(shown));
}

#[test]
fn opening_lists_each_file_once_in_order_and_closing_hides_them() {
    let Some(out) = open_run(vec![page(Value::Array(three_named())), projects()], vec![json!({"click": TOGGLE}), json!({"click": TOGGLE})]) else { return };
    let opened = frame(&out, 2);
    assert_eq!((count(opened, OPENED), count(opened, FILES)), (1, 2), "{out}");
    let texts = texts(opened);
    let at = |needle: &str| texts.iter().position(|t| t == needle).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("src/a.rs") < at("src/b.rs"), "{texts:?}");
    let closed = frame(&out, 3);
    assert_eq!((count(closed, CLOSED), count(closed, FILES)), (1, 0), "{out}");
}

#[test]
fn one_file_says_1_file() {
    let events = json!([event("e0", "stage.started", None), event("e1", "session.undeclared_files", Some(json!(["src/only.rs"]))), event("e2", "stage.completed", None)]);
    let Some(out) = open_run(vec![page(events), projects()], vec![]) else { return };
    assert!(texts(frame(&out, 1)).iter().any(|t| t == "Changed outside its features' files: 1 file"), "{:?}", texts(frame(&out, 1)));
}

#[test]
fn a_step_without_such_files_shows_no_line() {
    let events = json!([event("e0", "stage.started", None), event("e1", "stage.completed", None)]);
    let Some(out) = open_run(vec![page(events), projects()], vec![]) else { return };
    assert_eq!(count(frame(&out, 1), TOGGLE), 0, "{out}");
    assert!(!texts(frame(&out, 1)).iter().any(|t| t.starts_with("Changed outside")), "{out}");
}

/// A followed stream: an event as the server sends it, then the stream stays open.
fn stream(events: Vec<Value>, end: &str) -> Value {
    let chunks: Vec<String> = events.iter().map(|e| format!("id: {}\nevent: event\ndata: {e}\n\n", e["event_id"].as_str().unwrap())).collect();
    json!({"status": 200, "stream": chunks, "end": end})
}

#[test]
fn a_new_event_arriving_while_it_is_open_keeps_it_open() {
    // The first followed stream drops at once; the reopened one, after the list is opened, brings a new event.
    let arriving = stream(vec![event("e4", "session.note", None)], "hold");
    let responses = vec![page(Value::Array(three_named())), projects(), stream(vec![], "error"), arriving];
    let Some(out) = open_run(responses, vec![json!({"click": TOGGLE}), json!({"fire": 3000})]) else { return };
    assert!(texts(frame(&out, 3)).iter().any(|t| t == "session.note"), "the new event arrived: {:?}", texts(frame(&out, 3)));
    let after = frame(&out, 3);
    assert_eq!((count(after, OPENED), count(after, FILES)), (1, 2), "{out}");
}

#[test]
fn on_a_phone_an_opened_list_wraps_long_paths_without_scrolling_sideways() {
    let Some(chromium) = web_chromium::binary() else { return };
    let long = format!("crates/{}/src/{}.rs", "deeply-nested-directory-name".repeat(4), "x".repeat(120));
    let events = json!([event("e0", "stage.started", None), event("e1", "session.undeclared_files", Some(json!([long, "src/b.rs"]))), event("e2", "stage.completed", None)]);
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = [web_chromium::get("/api/projects", json!({"projects": [project]})), web_chromium::get("/api/projects/alpha/runs/run-1", page(events)["json"].clone())];
    let steps = json!([{"wait": TOGGLE}, {"click": TOGGLE}, {"wait": FILES}, {"boxes": FILES}]);
    let result = web_chromium::measure(&chromium, 360, "/projects/alpha/orchestrations/run-1", &replies, steps).result;
    assert!(result.get("error").is_none(), "{result}");
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "the page scrolls sideways: {result}");
    assert_eq!(result["offenders"], json!([]), "a path runs past the screen: {result}");
    let boxes = result["boxes"][FILES].as_array().unwrap();
    assert_eq!(boxes.len(), 2, "{result}");
    assert!(boxes.iter().all(|b| b["right"].as_i64().unwrap() <= 360), "{result}");
    assert!(boxes[0]["bottom"].as_i64().unwrap() - boxes[0]["top"].as_i64().unwrap() > boxes[1]["bottom"].as_i64().unwrap() - boxes[1]["top"].as_i64().unwrap(), "the long path wraps onto more lines: {result}");
}

#[test]
fn the_opened_list_stays_inside_its_step_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    let long = format!("crates/{}/src/{}.rs", "deeply-nested-directory-name".repeat(4), "x".repeat(120));
    let events = json!([event("e0", "stage.started", None), event("e1", "session.undeclared_files", Some(json!([long, "src/b.rs"]))), event("e2", "stage.completed", None)]);
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = [web_chromium::get("/api/projects", json!({"projects": [project]})), web_chromium::get("/api/projects/alpha/runs/run-1", page(events)["json"].clone())];
    const STEP: &str = ".run-step:has(.run-step-undeclared)";
    const GROUP: &str = ".run-step-undeclared";
    for width in [360, 1280] {
        let steps = json!([{"wait": TOGGLE}, {"click": TOGGLE}, {"wait": FILES}, {"boxes": STEP}, {"boxes": GROUP}]);
        let result = web_chromium::measure(&chromium, width, "/projects/alpha/orchestrations/run-1", &replies, steps).result;
        assert!(result.get("error").is_none(), "{width} px: {result}");
        let (step, group) = (&result["boxes"][STEP][0], &result["boxes"][GROUP][0]);
        assert!(group["right"].as_i64().unwrap() <= step["right"].as_i64().unwrap(), "{width} px: the list {group} ends past its step {step}");
        assert!(group["left"].as_i64().unwrap() > step["left"].as_i64().unwrap(), "{width} px: the list {group} is indented in its step {step}");
    }
}
