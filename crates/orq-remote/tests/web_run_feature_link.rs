//! A run's page and the runs table link to the run's feature, so its description, dependencies and
//! run history are one click away.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": ["a[data-run-feature]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn header(feature: &str) -> Value {
    json!({"run_id": "run-1", "feature": feature, "target_branch": "main", "state": "failed", "stage": "implement", "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"})
}

fn page(run: Value) -> Value {
    json!({"status": 200, "json": {"run": run, "events": [], "next_cursor": null}})
}

fn ledger(name: &str) -> Value {
    json!({"status": 200, "json": {"features": [{"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null}]}})
}

fn feature_links(frame: &Value) -> Vec<String> {
    frame["settled"]["links"].as_array().unwrap().iter().filter(|l| l["href"].as_str().unwrap_or("").contains("/features/")).filter_map(|l| l["href"].as_str()).map(String::from).collect()
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().filter_map(|r| r["url"].as_str()).map(String::from).collect()
}

#[test]
fn the_run_page_title_links_to_its_feature() {
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([page(header("login")), projects(), ledger("login")]), json!([{"submit": TOKEN}, {"nav": "a[data-run-feature]"}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["matches"]["a[data-run-feature]"], 1, "{out}");
    assert_eq!(feature_links(&out["frames"][1]), ["/projects/alpha/features/login"], "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/features/login", "{out}");
    assert!(urls(&out).iter().any(|u| u == "/api/projects/alpha/features"), "{out}");
}

#[test]
fn a_feature_name_with_spaces_or_slashes_is_encoded_in_the_link() {
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([page(header("a b/c")), projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(feature_links(&out["frames"][1]), ["/projects/alpha/features/a%20b%2Fc"], "{out}");
}

#[test]
fn a_plan_run_has_no_feature_link() {
    let mut plan = header("login");
    plan["kind"] = json!("plan");
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([page(plan), projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    assert_eq!(frame["settled"]["matches"]["a[data-run-feature]"], 0, "{out}");
    assert!(frame["settled"]["texts"].as_array().unwrap().iter().any(|t| t == "login"), "the name is still shown: {out}");
}

#[test]
fn each_run_row_links_to_its_feature_and_the_name_still_opens_the_run() {
    let list = json!({"status": 200, "json": {"runs": [header("login")]}});
    let Some(out) = run("/projects/alpha/orchestrations", json!([list.clone(), projects(), ledger("login")]), json!([{"submit": TOKEN}, {"nav": "a[data-run-feature]"}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["matches"]["a[data-run-feature]"], 1, "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/features/login", "{out}");
    let Some(out) = run("/projects/alpha/orchestrations", json!([list, projects(), page(header("login"))]), json!([{"submit": TOKEN}, {"click": "button[data-open]", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/orchestrations/run-1", "{out}");
}

#[test]
fn the_all_projects_runs_list_links_each_feature_in_its_own_project() {
    let runs = |run: &str, feature: &str| json!({"status": 200, "json": {"runs": [{"run_id": run, "feature": feature, "state": "failed", "stage": null, "started_at": null, "updated_at": null, "target_branch": null}]}});
    let Some(out) = run("/orchestrations", json!([projects(), runs("run-a", "login"), runs("run-b", "signup")]), json!([{"submit": TOKEN}])) else { return };
    let mut links = feature_links(&out["frames"][1]);
    links.sort();
    assert_eq!(links, ["/projects/alpha/features/login", "/projects/beta/features/signup"], "{out}");
}
