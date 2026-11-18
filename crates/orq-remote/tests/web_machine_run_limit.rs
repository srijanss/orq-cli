//! This machine's run limits on the page: the dashboard says "Runs: N of M on this machine" and the
//! free disk while either `[runs]` limit is set, read-only and nothing at all without one; a project
//! the machine refuses a start has its Start disabled with the machine's reason beside it.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const REASON: &str = "this machine already has 2 active runs (alpha run-1, beta run-2): wait for one to finish, or raise [runs] max_active";
const SELECTORS: [&str; 6] = ["[data-machine-runs]", "[data-machine-runs] input", "[data-machine-runs] button", "[data-start]", "[data-start][disabled]", "[data-start-blocked]"];

fn run(url: &str, responses: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn project(id: &str, active: u64, blocked: Value) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 2, "active_runs": active, "max_active_runs": null, "start_blocked": blocked, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn listing(machine: Value) -> Value {
    json!({"status": 200, "json": {"projects": [project("alpha", 1, json!(REASON)), project("beta", 1, json!(REASON))], "machine": machine}})
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_dashboard_says_how_many_runs_of_the_machines_limit_are_active_and_its_free_disk() {
    let machine = json!({"active_runs": 2, "max_active_runs": 2, "min_free_disk_gb": 15, "free_disk_gb": 9.8});
    let Some(out) = run("/dashboard", vec![listing(machine)]) else { return };
    let frame = last(&out);
    let shown = texts(frame);
    assert!(shown.iter().any(|t| t == "Runs: 2 of 2 on this machine"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "9.8 GB free, 15 GB needed to start"), "{shown:?}");
    assert_eq!((count(frame, "[data-machine-runs]"), count(frame, "[data-machine-runs] input"), count(frame, "[data-machine-runs] button")), (1, 0, 0), "read-only: {out}");

    let disk_only = json!({"active_runs": 2, "max_active_runs": null, "min_free_disk_gb": null, "free_disk_gb": 40.0});
    let Some(out) = run("/dashboard", vec![listing(disk_only)]) else { return };
    let shown = texts(last(&out));
    assert!(shown.iter().any(|t| t == "Runs: 2 on this machine") && shown.iter().any(|t| t == "40 GB free"), "{shown:?}");

    let Some(out) = run("/dashboard", vec![listing(Value::Null)]) else { return };
    assert_eq!(count(last(&out), "[data-machine-runs]"), 0, "no limit, no line: {out}");
}

#[test]
fn a_start_the_machine_refuses_is_disabled_with_its_reason() {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    let ledger = json!({"status": 200, "json": {"features": [feature("COM-01")], "remote_actions": true, "start_blocked": REASON}});
    let no_runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true}});
    let Some(out) = run("/projects/alpha/features/COM-01", vec![ledger, no_runs, listing(Value::Null)]) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, "[data-start]"), count(frame, "[data-start][disabled]")), (1, 1), "{out}");
    assert!(texts(frame).iter().any(|t| t == REASON), "{:?}", texts(frame));
}
