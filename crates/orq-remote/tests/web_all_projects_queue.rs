//! The Queue page across every project, at /queue: one group per project that has something queued or is
//! paused, in project order, headed by the project's name, how many are queued and "Paused: …", linking to
//! that project's queue page. It only shows the queues: reordering, removing and pausing stay on each
//! project's own page. A project whose queue cannot be read says so in its group; every queue empty says
//! "Nothing is queued". The project pickers move between the projects' queue pages.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

const SELECTORS: &[&str] = &[
    "[data-queue-group]",
    "[data-queue-group=\"alpha\"]",
    "[data-queue-group=\"beta\"]",
    "[data-queue-group=\"gamma\"]",
    "[data-queue-group=\"alpha\"] [data-queue-entry]",
    "[data-queue-group=\"alpha\"] [data-queue-group-error]",
    "[data-queue-group=\"beta\"] [data-queue-paused]",
    "[data-queue-entry]",
    "[data-queue-up]",
    "[data-queue-down]",
    "[data-queue-remove]",
    "[data-queue-pause]",
    "[data-queue-resume]",
    "[data-queue-empty]",
];

fn run(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn project(id: &str) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": null})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta"), project("gamma")]}})
}

fn entry(feature: &str, state: &str, reason: Value) -> Value {
    json!({"feature": feature, "state": state, "run": null, "reason": reason, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null})
}

/// A project's queue reply, with remote actions on so that leaving the controls out is this page's choice.
fn queue(entries: Vec<Value>, paused: Value) -> Value {
    json!({"status": 200, "json": {"queue": {"entries": entries, "count": entries.len(), "paused": paused}, "remote_actions": true}})
}

fn empty() -> Value {
    queue(vec![], Value::Null)
}

fn two_queued() -> Value {
    queue(vec![entry("COM-01", "next", Value::Null), entry("COM-02", "waiting", json!("waiting for COM-01"))], Value::Null)
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} is not measured"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn shows(frame: &Value, wanted: &str) -> bool {
    texts(frame).iter().any(|t| t.contains(wanted))
}

fn links(frame: &Value) -> Vec<String> {
    frame["settled"]["links"].as_array().unwrap().iter().filter_map(|l| l["href"].as_str()).map(String::from).collect()
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().filter_map(|r| r["url"].as_str()).map(String::from).collect()
}

#[test]
fn the_page_groups_the_queued_and_the_paused_projects_in_project_order_and_leaves_out_the_empty_ones() {
    let Some(out) = run("/queue", vec![projects(), two_queued(), queue(vec![], json!("paused from the web")), empty()], vec![]) else { return };
    assert_eq!(urls(&out)[..4], ["/api/projects", "/api/projects/alpha/queue", "/api/projects/beta/queue", "/api/projects/gamma/queue"], "{out}");
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-group]"), 2, "{out}");
    assert_eq!((count(frame, "[data-queue-group=\"alpha\"]"), count(frame, "[data-queue-group=\"beta\"]"), count(frame, "[data-queue-group=\"gamma\"]")), (1, 1, 0), "{out}");
    assert_eq!(count(frame, "[data-queue-group=\"alpha\"] [data-queue-entry]"), 2, "{out}");
    assert_eq!(count(frame, "[data-queue-group=\"beta\"] [data-queue-paused]"), 1, "{out}");
    let texts = texts(frame);
    let at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("2 queued") < at("Paused: paused from the web"), "alpha's group comes before beta's: {texts:?}");
    assert!(at("COM-01") < at("COM-02"), "{texts:?}");
    for shown in ["Next", "Waiting", "waiting for COM-01"] {
        assert!(texts.iter().any(|t| t.contains(shown)), "{shown}: {texts:?}");
    }
    let links = links(frame);
    assert!(links.contains(&"/projects/alpha/queue".to_string()) && links.contains(&"/projects/beta/queue".to_string()), "each group links to its project's queue: {links:?}");
    assert!(!links.contains(&"/projects/gamma/queue".to_string()), "{links:?}");
}

#[test]
fn the_page_only_shows_the_queues_and_offers_no_reorder_remove_or_pause() {
    let Some(out) = run("/queue", vec![projects(), two_queued(), queue(vec![entry("COM-09", "next", Value::Null)], json!("paused")), empty()], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-entry]"), 3, "{out}");
    for control in ["[data-queue-up]", "[data-queue-down]", "[data-queue-remove]", "[data-queue-pause]", "[data-queue-resume]"] {
        assert_eq!(count(frame, control), 0, "{control}: {out}");
    }
}

#[test]
fn with_every_queue_empty_the_page_says_nothing_is_queued_and_where_to_queue_from() {
    let Some(out) = run("/queue", vec![projects(), empty(), empty(), empty()], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-group]"), 0, "{out}");
    assert_eq!(count(frame, "[data-queue-empty]"), 1, "{out}");
    assert!(shows(frame, "Nothing is queued"), "{:?}", texts(frame));
    assert!(shows(frame, "Features page"), "a hint to queue pending features from a project's Features page: {:?}", texts(frame));
}

#[test]
fn a_project_whose_queue_cannot_be_read_says_so_in_its_group_and_the_others_still_show() {
    let Some(out) = run("/queue", vec![projects(), json!({"status": 500, "json": {}}), queue(vec![entry("COM-05", "next", Value::Null)], Value::Null), empty()], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-group=\"alpha\"] [data-queue-group-error]"), 1, "{out}");
    assert!(shows(frame, "could not be read"), "{:?}", texts(frame));
    assert_eq!(count(frame, "[data-queue-group=\"beta\"]"), 1, "{out}");
    assert!(shows(frame, "COM-05"), "{:?}", texts(frame));
    assert!(!shows(frame, "Nothing is queued"), "an unread queue is not an empty one: {:?}", texts(frame));
}

#[test]
fn the_page_is_titled_queue_and_announced_as_the_queue_page() {
    let Some(out) = run("/queue", vec![projects(), empty(), empty(), empty()], vec![]) else { return };
    let frame = last(&out);
    assert!(texts(frame).iter().any(|t| t == "Queue"), "{:?}", texts(frame));
    let live = frame["settled"]["a11y"]["live"].as_array().unwrap();
    assert!(live.iter().any(|l| l["text"] == "Queue page"), "{live:?}");
}

#[test]
fn a_project_queue_page_is_announced_as_that_projects_queue() {
    let Some(out) = run("/projects/alpha/queue", vec![two_queued(), projects()], vec![]) else { return };
    let live = last(&out)["settled"]["a11y"]["live"].as_array().unwrap().clone();
    assert!(live.iter().any(|l| l["text"] == "Queue, project alpha"), "{live:?}");
}

#[test]
fn the_page_picker_on_the_all_projects_queue_opens_the_chosen_projects_queue() {
    let steps = vec![json!({"click": "main [data-project-select]", "index": 0}), json!({"click": "main [data-project-choice=beta]", "index": 0})];
    let Some(out) = run("/queue", vec![projects(), empty(), empty(), empty(), two_queued(), projects()], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/projects/beta/queue", "{out}");
}

#[test]
fn the_page_picker_on_a_project_queue_moves_to_another_projects_queue_and_back_to_all() {
    let steps = vec![json!({"click": "main [data-project-select]", "index": 0}), json!({"click": "main [data-project-choice=beta]", "index": 0})];
    let Some(out) = run("/projects/alpha/queue", vec![two_queued(), projects(), two_queued(), projects()], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/projects/beta/queue", "{out}");
    let steps = vec![json!({"click": "main [data-project-select]", "index": 0}), json!({"click": "main [data-project-choice=\"\"]", "index": 0})];
    let Some(out) = run("/projects/alpha/queue", vec![two_queued(), projects(), projects(), empty(), empty(), empty()], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/queue", "{out}");
}

#[test]
fn the_sidebar_project_switcher_keeps_the_queue_section() {
    let steps = vec![json!({"nav": "[data-project-switcher]"}), json!({"select": "beta"})];
    let Some(out) = run("/queue", vec![projects(), empty(), empty(), empty(), projects(), two_queued(), projects()], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/projects/beta/queue", "{out}");
    let steps = vec![json!({"nav": "[data-project-switcher]"}), json!({"select": ""})];
    let Some(out) = run("/projects/alpha/queue", vec![two_queued(), projects(), projects(), empty(), empty(), empty()], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/queue", "{out}");
}
