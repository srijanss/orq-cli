//! Pulling a project's target on the web: the settings' "Pull main after an MR merges and before each
//! run" (on by default, sent with Save), a target that could not be pulled after a merge as a warning
//! on the dashboard card and on the project's Queue page with a Retry that pulls again, and a run whose
//! start could not pull its target saying so on its page.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 8] = [
    "[data-pull-target-before-runs]",
    "[data-project-pull-target]",
    "[data-target-pull-warning]",
    "[data-target-pull-retry]",
    "[data-card-target-pull]",
    "[data-run-target-pull]",
    "[data-confirm]",
    "[data-project-settings-save]",
];
const WARNING: &str = "main not updated after !12 merged: main has 1 commit not on origin and origin has 1 not here";

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

fn project(queue: Value) -> Value {
    json!({"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": queue})
}

fn projects(queue: Value) -> Value {
    json!({"status": 200, "json": {"projects": [project(queue)]}})
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

fn settings(pull: Option<bool>) -> Value {
    let mut settings = json!({
        "implementer": {"project": null, "fallback": null},
        "reviewer": {"project": null, "fallback": null},
        "delivery": "review",
        "max_active_runs": null,
        "min_free_disk_gb": null,
        "queue_hours": null,
        "queue_stop_on_failure": false,
        "queue_wait_for_review": true,
        "target_branch": "main",
        "revision": 3
    });
    if let Some(pull) = pull {
        settings["pull_target_before_runs"] = json!(pull);
    }
    json!({"status": 200, "json": {"settings": settings}})
}

#[test]
fn settings_offer_pulling_the_target_on_by_default_and_save_sends_it_turned_off() {
    let Some(out) = run("/projects/alpha/settings", vec![settings(None), projects(Value::Null)], vec![]) else { return };
    let frame = last(&out);
    assert!(shows(frame, "Pull main after an MR merges and before each run"), "{:?}", texts(frame));
    assert_eq!(count(frame, "[data-project-pull-target]"), 1, "{out}");
    assert!(shows(frame, "Fast-forwards main first"), "an unsaid setting is on: {:?}", texts(frame));

    let machine = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
    let options = json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus"]}]}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = vec![
        json!({"click": "[data-project-settings-edit]"}),
        json!({"click": "[data-pull-target-before-runs]"}),
        json!({"click": "[data-project-settings-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let responses = vec![settings(Some(true)), projects(Value::Null), machine, options, ok, settings(Some(false))];
    let Some(out) = run("/projects/alpha/settings", responses, steps) else { return };
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0].1["pull_target_before_runs"], false, "{out}");
    assert!(shows(last(&out), "Leaves main as it is"), "{:?}", texts(last(&out)));
}

fn queue_reply(warning: Value) -> Value {
    let mut queue = json!({"entries": [], "count": 0, "paused": null});
    if !warning.is_null() {
        queue["target_pull_warning"] = warning;
    }
    json!({"status": 200, "json": {"queue": queue, "remote_actions": true}})
}

#[test]
fn a_target_that_could_not_be_pulled_warns_on_the_queue_page_and_retry_pulls_it_again() {
    let pulled = json!({"status": 200, "json": {"outcome": "fast_forwarded", "message": "main fast-forwarded from 1111aaaa to 2222bbbb", "target_pull_warning": null}});
    let steps = vec![json!({"click": "[data-target-pull-retry]"})];
    let Some(out) = run("/projects/alpha/queue", vec![queue_reply(json!(WARNING)), projects(Value::Null), pulled, queue_reply(Value::Null)], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    let warned = &frames[1];
    assert_eq!((count(warned, "[data-target-pull-warning]"), count(warned, "[data-target-pull-retry]")), (1, 1), "{out}");
    assert!(shows(warned, WARNING), "{:?}", texts(warned));
    assert_eq!(posts(&out), vec![("/api/projects/alpha/pull".to_string(), json!({}))], "{out}");
    let frame = last(&out);
    assert_eq!(count(frame, "[data-target-pull-warning]"), 0, "a pull that worked clears the warning: {out}");
    assert!(shows(frame, "main fast-forwarded from 1111aaaa to 2222bbbb"), "{:?}", texts(frame));
}

#[test]
fn a_retry_that_still_cannot_pull_keeps_the_warning_with_the_new_reason() {
    let again = "main not updated after !12 merged: main has uncommitted changes to src/lib.rs";
    let refused = json!({"status": 200, "json": {"outcome": "dirty", "message": "main has uncommitted changes to src/lib.rs", "target_pull_warning": again}});
    let steps = vec![json!({"click": "[data-target-pull-retry]"})];
    let Some(out) = run("/projects/alpha/queue", vec![queue_reply(json!(WARNING)), projects(Value::Null), refused], steps) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-target-pull-warning]"), 1, "{out}");
    assert!(shows(frame, again), "{:?}", texts(frame));
}

#[test]
fn the_dashboard_card_warns_of_a_target_that_could_not_be_pulled() {
    let Some(out) = run("/dashboard", vec![projects(json!({"count": 0, "paused": false, "target_pull_warning": WARNING}))], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-card-target-pull]"), 1, "{out}");
    assert!(shows(frame, WARNING), "{:?}", texts(frame));

    let Some(out) = run("/dashboard", vec![projects(json!({"count": 0, "paused": false}))], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-card-target-pull]"), 0, "{out}");
}

#[test]
fn a_run_whose_start_could_not_pull_its_target_says_so_on_its_page() {
    let shown = json!({"run_id": "run-7", "feature": "COM-01", "state": "running", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    let warning = json!({"event_id": "run-7-target-pull", "kind": "target.pull_warning", "stage": null, "attempt": 1, "occurred_at": "2026-11-19T10:00:00Z",
        "target_pull": {"outcome": "diverged", "message": "main has 1 commit not on origin and origin has 1 not here"}});
    let body = json!({"status": 200, "json": {"run": shown, "events": [warning], "next_cursor": null, "remote_actions": true}});
    let responses = vec![body, projects(Value::Null), json!({"status": 200, "stream": [], "end": "hold"})];
    let Some(out) = run("/projects/alpha/orchestrations/run-7", responses, vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-run-target-pull]"), 1, "{out}");
    assert!(shows(frame, "main was not pulled before this run started: main has 1 commit not on origin and origin has 1 not here"), "{:?}", texts(frame));
    assert!(shows(frame, "It started from the local main."), "{:?}", texts(frame));
}
