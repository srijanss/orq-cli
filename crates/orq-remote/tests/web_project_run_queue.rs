//! A project's run queue on the page: its Queue section lists the entries in the order they start, each
//! waiting, next, queued, running (with its run) or failed (with its run), with Pause/Resume, Remove and
//! ↑/↓ to reorder, all only while remote actions are on; the dashboard card says only "N Queued";
//! a pending feature's page offers Queue, the Features list offers "Queue N" for the marked features; and
//! the project's settings show and save the queue hours and whether a failed run pauses the queue.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 24] = [
    "[data-queue-entry]",
    "[data-queue-link]",
    "[data-queue-state=\"next\"]",
    "[data-queue-state=\"waiting\"]",
    "[data-queue-state=\"running\"]",
    "[data-queue-state=\"failed\"]",
    "[data-open-run]",
    "[data-queue-pause]",
    "[data-queue-resume]",
    "[data-queue-paused]",
    "[data-queue-up]",
    "[data-queue-down]",
    "[data-queue-remove]",
    "[data-queue-feature]",
    "[data-queue-marked]",
    "[data-card-stat=\"alpha/queue\"]",
    "[data-card-stat=\"beta/queue\"]",
    "[data-confirm]",
    "[data-queue-hours]",
    "[data-queue-stop-on-failure]",
    "[data-project-queue-hours]",
    "[data-project-queue-stop]",
    "[data-queue-hours-invalid]",
    "[data-project-settings-save][disabled]",
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

fn entry(feature: &str, state: &str, run: Value, reason: Value) -> Value {
    json!({"feature": feature, "state": state, "run": run, "reason": reason, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null})
}

fn queue(entries: Vec<Value>, paused: Value) -> Value {
    json!({"entries": entries, "count": entries.len(), "paused": paused})
}

/// The queue the page opens on: a failed COM-01 with its run, a running COM-04, COM-03 next and COM-02
/// waiting for COM-01.
fn opened() -> Value {
    queue(
        vec![
            entry("COM-01", "failed", json!("run-1-COM-01"), Value::Null),
            entry("COM-04", "running", json!("run-2-COM-04"), Value::Null),
            entry("COM-03", "next", Value::Null, Value::Null),
            entry("COM-02", "waiting", Value::Null, json!("waiting for COM-01")),
        ],
        Value::Null,
    )
}

fn reply(queue: Value, actions: bool) -> Value {
    json!({"status": 200, "json": {"queue": queue, "remote_actions": actions}})
}

fn project(id: &str, queue: Value) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": queue})
}

fn projects(list: Vec<Value>) -> Value {
    json!({"status": 200, "json": {"projects": list}})
}

fn ledger() -> Value {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("COM-01"), feature("COM-03")], "remote_actions": true, "start_blocked": null}})
}

fn no_runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn shows(frame: &Value, wanted: &str) -> bool {
    texts(frame).iter().any(|t| t.contains(wanted))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<(String, Value)> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "POST")
        .map(|r| (r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap_or("null")).unwrap()))
        .collect()
}

#[test]
fn the_queue_section_shows_each_entry_s_state_and_its_run() {
    let Some(out) = run("/projects/alpha/queue", vec![reply(opened(), true), projects(vec![project("alpha", json!({"count": 4, "paused": false}))])], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-entry]"), 4, "{out}");
    for state in ["next", "waiting", "running", "failed"] {
        assert_eq!(count(frame, &format!("[data-queue-state=\"{state}\"]")), 1, "{state}: {out}");
    }
    assert_eq!(count(frame, "[data-open-run]"), 2, "the failed and the running entry link their runs: {out}");
    assert!(shows(frame, "waiting for COM-01"), "{:?}", texts(frame));
    assert_eq!((count(frame, "[data-queue-pause]"), count(frame, "[data-queue-resume]")), (1, 0), "{out}");
    assert_eq!((count(frame, "[data-queue-up]"), count(frame, "[data-queue-down]"), count(frame, "[data-queue-remove]")), (4, 4, 4), "{out}");
}

#[test]
fn reorder_pause_resume_and_remove_send_their_changes_and_show_the_queue_the_server_returns() {
    let paused = queue(vec![entry("COM-03", "next", Value::Null, Value::Null)], json!("paused from the web"));
    let steps = vec![
        json!({"click": "[data-queue-down]", "index": 0}),
        json!({"click": "[data-queue-pause]"}),
        json!({"click": "[data-queue-resume]"}),
        json!({"click": "[data-queue-remove]", "index": 0}),
        json!({"click": "[data-confirm]"}),
    ];
    let responses = vec![reply(opened(), true), projects(vec![project("alpha", Value::Null)]), reply(opened(), true), reply(paused.clone(), true), reply(opened(), true), reply(paused, true)];
    let Some(out) = run("/projects/alpha/queue", responses, steps) else { return };
    assert_eq!(
        posts(&out),
        vec![
            ("/api/projects/alpha/queue/reorder".to_string(), json!({"order": ["COM-04", "COM-01", "COM-03", "COM-02"]})),
            ("/api/projects/alpha/queue/pause".to_string(), json!({})),
            ("/api/projects/alpha/queue/resume".to_string(), json!({})),
            ("/api/projects/alpha/queue/remove".to_string(), json!({"feature": "COM-01"})),
        ],
        "{out}"
    );
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-entry]"), 1, "the reply's queue is shown: {out}");
    assert_eq!((count(frame, "[data-queue-paused]"), count(frame, "[data-queue-resume]")), (1, 1), "{out}");
    assert!(shows(frame, "paused from the web"), "{:?}", texts(frame));
}

#[test]
fn with_remote_actions_off_the_queue_is_shown_without_its_controls() {
    let Some(out) = run("/projects/alpha/queue", vec![reply(opened(), false), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-entry]"), 4, "{out}");
    for control in ["[data-queue-pause]", "[data-queue-resume]", "[data-queue-up]", "[data-queue-down]", "[data-queue-remove]"] {
        assert_eq!(count(frame, control), 0, "{control}: {out}");
    }
}

#[test]
fn the_dashboard_card_says_only_how_many_are_queued() {
    let listing = projects(vec![project("alpha", json!({"count": 3, "paused": true})), project("beta", json!({"count": 0, "paused": false}))]);
    let Some(out) = run("/dashboard", vec![listing], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-card-stat=\"alpha/queue\"]"), count(frame, "[data-card-stat=\"beta/queue\"]")), (1, 1), "an empty queue shows 0: {out}");
    let texts = texts(frame);
    let alpha = texts.iter().position(|t| t == "alpha").unwrap();
    assert_eq!(texts[alpha + 5..alpha + 7], ["3", "Queued"], "{texts:?}");
    let beta = texts.iter().position(|t| t == "beta").unwrap();
    assert_eq!(texts[beta + 5..beta + 7], ["0", "Queued"], "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("paused")), "whether it is paused stays on the Queue page: {texts:?}");
}

#[test]
fn a_pending_feature_is_queued_from_its_page_after_asking() {
    let queued = json!({"status": 200, "json": {"queue": queue(vec![entry("COM-01", "next", Value::Null, Value::Null)], Value::Null)}});
    let steps = vec![json!({"click": "[data-queue-feature]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger(), no_runs(), projects(vec![project("alpha", Value::Null)]), queued], steps) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/queue".to_string(), json!({"features": ["COM-01"]}))], "{out}");
    assert!(shows(last(&out), "Queued COM-01."), "{:?}", texts(last(&out)));

    let refused = json!({"status": 409, "json": {"error": "those features are queued already"}});
    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger(), no_runs(), projects(vec![project("alpha", Value::Null)]), refused], vec![json!({"click": "[data-queue-feature]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(shows(last(&out), "those features are queued already"), "{:?}", texts(last(&out)));
}

#[test]
fn the_marked_features_are_queued_together_from_the_features_list() {
    let queued = json!({"status": 200, "json": {"queue": queue(vec![], Value::Null)}});
    let steps = vec![json!({"click": "[data-mark=\"COM-01\"]"}), json!({"click": "[data-mark=\"COM-03\"]"}), json!({"click": "[data-queue-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features", vec![ledger(), projects(vec![project("alpha", Value::Null)]), queued], steps) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/queue".to_string(), json!({"features": ["COM-01", "COM-03"]}))], "{out}");
    assert!(shows(last(&out), "Queued COM-01, COM-03."), "{:?}", texts(last(&out)));
}

fn project_settings(hours: Value, stop: bool) -> Value {
    json!({"status": 200, "json": {"settings": {
        "implementer": {"project": null, "fallback": null},
        "reviewer": {"project": null, "fallback": null},
        "delivery": "review",
        "max_active_runs": null,
        "min_free_disk_gb": null,
        "queue_hours": hours,
        "queue_stop_on_failure": stop,
        "revision": 3
    }}})
}

#[test]
fn settings_show_the_queue_hours_and_stop_on_failure_and_save_sends_both() {
    let Some(out) = run("/projects/alpha/settings", vec![project_settings(json!("22:00-07:00"), true), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
    let frame = last(&out);
    assert!(texts(frame).iter().any(|t| t == "22:00-07:00"), "{:?}", texts(frame));
    assert!(shows(frame, "When a run fails") && shows(frame, "Pauses the queue"), "{:?}", texts(frame));

    let machine = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
    let options = json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus"]}]}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = vec![
        json!({"click": "[data-project-settings-edit]"}),
        json!({"set": "[data-queue-hours]", "value": "22:00"}),
        json!({"set": "[data-queue-hours]", "value": "09:00-17:30"}),
        json!({"click": "[data-queue-stop-on-failure]"}),
        json!({"click": "[data-project-settings-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let responses = vec![project_settings(Value::Null, false), projects(vec![project("alpha", Value::Null)]), machine, options, ok, project_settings(json!("09:00-17:30"), true)];
    let Some(out) = run("/projects/alpha/settings", responses, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    let typed_bad = frames.iter().find(|frame| count(frame, "[data-queue-hours-invalid]") == 1).unwrap_or_else(|| panic!("hours that are not a window are pointed out: {out}"));
    assert_eq!(count(typed_bad, "[data-project-settings-save][disabled]"), 1, "and hold Save back: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(
        sent[0].1,
        json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "queue_hours": "09:00-17:30", "queue_stop_on_failure": true, "queue_wait_for_review": true, "revision": 3})
    );
}

#[test]
fn the_features_list_has_no_queue_link_and_the_sidebar_reaches_the_projects_queue() {
    let Some(out) = run("/projects/alpha/features", vec![ledger(), projects(vec![project("alpha", Value::Null)])], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-link]"), 0, "{out}");
    let sidebar: Vec<&str> = frame["settled"]["sidebarLinks"].as_array().unwrap().iter().filter_map(|l| l.as_str()).collect();
    assert!(sidebar.contains(&"/projects/alpha/queue"), "{sidebar:?}");
}
