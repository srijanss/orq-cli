//! A project's run limit and disk floor on the page: its Settings offer "At most N active runs" and
//! "Free disk needed to start (GB)", each empty for no limit, and Save sends both; while the project
//! may not start a run, each Start (the feature page's Start and Start with…, the Features list's
//! Start N, across every project too) is disabled with the reason beside it; a stale page's Start
//! shows the server's 409 reason; and the dashboard card says "1 of 1" runs active under a limit.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const REASON: &str = "alpha already has 1 active run (run-seed, COM-02): wait for it to finish, or raise the limit in project settings";
const SELECTORS: [&str; 12] = [
    "[data-start]",
    "[data-start][disabled]",
    "[data-start-with]",
    "[data-start-with][disabled]",
    "[data-start-marked]",
    "[data-start-marked][disabled]",
    "[data-start-blocked]",
    "[data-max-active-runs]",
    "[data-min-free-disk]",
    "[data-project-settings-save][disabled]",
    "[data-min-free-disk-invalid]",
    "[data-confirm]",
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

fn project(id: &str, active: u64, max: Value, blocked: Value) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 2, "active_runs": active, "max_active_runs": max, "start_blocked": blocked, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn projects(list: Vec<Value>) -> Value {
    json!({"status": 200, "json": {"projects": list}})
}

/// alpha's ledger, COM-01 and COM-03 pending, with remote actions on and `blocked` as the reply's start_blocked.
fn ledger(blocked: Value) -> Value {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("COM-01"), feature("COM-03")], "remote_actions": true, "start_blocked": blocked}})
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

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn on_a_blocked_projects_feature_page_start_and_start_with_are_disabled_with_the_reason() {
    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger(json!(REASON)), no_runs(), projects(vec![project("alpha", 1, json!(1), json!(REASON))])], vec![json!({"click": "[data-start]"})]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-start]"), count(frame, "[data-start][disabled]")), (1, 1), "{out}");
    assert_eq!((count(frame, "[data-start-with]"), count(frame, "[data-start-with][disabled]")), (1, 1), "{out}");
    assert!(texts(frame).iter().any(|t| t == REASON), "the reason is shown: {:?}", texts(frame));
    assert_eq!(count(frame, "[data-confirm]"), 0, "a disabled Start asks nothing: {out}");
    assert!(posts(&out).is_empty(), "{out}");

    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger(Value::Null), no_runs(), projects(vec![project("alpha", 0, json!(1), Value::Null)])], vec![]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-start]"), count(frame, "[data-start][disabled]"), count(frame, "[data-start-blocked]")), (1, 0, 0), "free to start: {out}");
}

#[test]
fn a_stale_pages_start_shows_the_servers_409_reason() {
    let refused = json!({"status": 409, "json": {"error": REASON}});
    let steps = vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger(Value::Null), no_runs(), projects(vec![project("alpha", 0, json!(1), Value::Null)]), refused], steps) else { return };
    assert_eq!(posts(&out).len(), 1, "{out}");
    assert!(texts(last(&out)).iter().any(|t| t == REASON), "{:?}", texts(last(&out)));
}

#[test]
fn the_features_lists_start_n_is_disabled_with_the_reason_in_the_project_and_across_projects() {
    let mark = json!({"click": "[data-mark=\"COM-01\"]"});
    let Some(out) = run("/projects/alpha/features", vec![ledger(json!(REASON)), projects(vec![project("alpha", 1, json!(1), json!(REASON))])], vec![mark.clone()]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-start-marked]"), count(frame, "[data-start-marked][disabled]")), (1, 1), "{out}");
    assert!(texts(frame).iter().any(|t| t == REASON), "{:?}", texts(frame));

    let listing = projects(vec![project("alpha", 1, json!(1), json!(REASON)), project("beta", 0, Value::Null, Value::Null)]);
    let beta = json!({"status": 200, "json": {"features": [{"name": "B-01", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": true}});
    let both = vec![mark, json!({"click": "[data-mark=\"B-01\"]"})];
    let Some(out) = run("/features", vec![listing, ledger(Value::Null), beta], both) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-start-marked]"), count(frame, "[data-start-marked][disabled]")), (2, 1), "only alpha's is held back, by the listing's reason: {out}");
    assert_eq!(count(frame, "[data-start-blocked]"), 1, "{out}");
}

#[test]
fn the_dashboard_card_says_how_many_of_the_limit_are_active() {
    let listing = projects(vec![project("alpha", 1, json!(1), json!(REASON)), project("beta", 2, Value::Null, Value::Null)]);
    let Some(out) = run("/dashboard", vec![listing], vec![]) else { return };
    let shown = texts(last(&out));
    assert!(shown.iter().any(|t| t == "1 of 1"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "runs active"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "Active runs"), "no limit, the plain count: {shown:?}");
}

fn project_settings(max: Value, floor: Value, revision: u64) -> Value {
    json!({"status": 200, "json": {"settings": {
        "implementer": {"project": null, "fallback": null},
        "reviewer": {"project": null, "fallback": null},
        "delivery": "review",
        "max_active_runs": max,
        "min_free_disk_gb": floor,
        "revision": revision
    }}})
}

#[test]
fn settings_show_both_limits_edit_offers_no_limit_and_save_sends_both() {
    let machine = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
    let options = json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus"]}]}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = vec![
        json!({"click": "[data-project-settings-edit]"}),
        json!({"set": "[data-min-free-disk]", "value": "501"}),
        json!({"set": "[data-min-free-disk]", "value": "20"}),
        json!({"set": "[data-max-active-runs]", "value": ""}),
        json!({"click": "[data-project-settings-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let listing = projects(vec![project("alpha", 0, json!(2), Value::Null)]);
    let responses = vec![project_settings(json!(2), json!(15), 3), listing, machine, options, ok, project_settings(Value::Null, json!(20), 4)];
    let Some(out) = run("/projects/alpha/settings", responses, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    let first = texts(&frames[1]);
    for expected in ["At most N active runs", "At most 2 active runs", "Free disk needed to start (GB)", "15 GB"] {
        assert!(first.iter().any(|t| t == expected), "{expected}: {first:?}");
    }
    assert_eq!((count(&frames[2], "[data-max-active-runs]"), count(&frames[2], "[data-min-free-disk]")), (1, 1), "{out}");
    assert_eq!((count(&frames[3], "[data-min-free-disk-invalid]"), count(&frames[3], "[data-project-settings-save][disabled]")), (1, 1), "501 is refused before Save: {out}");
    assert_eq!(count(&frames[4], "[data-project-settings-save][disabled]"), 0, "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": 20, "revision": 3}));
    let after = texts(last(&out));
    assert!(after.iter().any(|t| t == "No limit") && after.iter().any(|t| t == "20 GB"), "{after:?}");
}
