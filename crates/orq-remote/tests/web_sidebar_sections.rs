//! The sidebar always offers Features, Orchestrations and Reviews. With no project chosen each one
//! lists the items of every project, grouped under the project's name.

mod web_deps;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn shell(scenario: Value) -> Option<Value> {
    web_deps::ready().then(|| {
        let output = Command::new("node").arg(crate_dir().join("tests/web_shell_run.mjs")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let dir = crate_dir();
    let output = Command::new("node").arg(dir.join("tests/web_react_harness.mjs")).arg(dir.join("web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn urls(out: &Value) -> Vec<String> {
    out["requests"].as_array().unwrap().iter().filter_map(|r| r["url"].as_str()).map(String::from).collect()
}

#[test]
fn without_a_project_the_sidebar_still_offers_features_orchestrations_and_reviews() {
    let Some(out) = shell(json!({"path": "/dashboard"})) else { return };
    let links = out["frames"][0]["links"].as_array().unwrap();
    let pairs: Vec<(&str, &str)> = links.iter().map(|l| (l["text"].as_str().unwrap(), l["href"].as_str().unwrap())).collect();
    assert_eq!(pairs, [("Dashboard", "/dashboard"), ("Features", "/features"), ("Orchestrations", "/orchestrations"), ("Reviews", "/reviews"), ("Settings", "/settings")], "{out}");
}

#[test]
fn the_all_projects_features_page_lists_every_projects_ledger_in_one_table() {
    let ledger = |name: &str| json!({"status": 200, "json": {"features": [{"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null}]}});
    let Some(out) = run("/features", json!([projects(), ledger("alpha-feature"), ledger("beta-feature")]), json!([{"submit": TOKEN}])) else { return };
    let urls = urls(&out);
    assert_eq!(urls[..3], ["/api/projects", "/api/projects/alpha/features", "/api/projects/beta/features"], "{out}");
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().rposition(|t| t == needle).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("alpha-feature") < at("beta-feature"), "{texts:?}");
    assert!(!texts.iter().any(|t| t == "Project"), "same columns as a single project: {texts:?}");
    assert_eq!(out["frames"][1]["settled"]["path"], "/features");
}

#[test]
fn the_all_projects_orchestrations_page_lists_every_projects_runs_under_its_name() {
    let runs = |run: &str| json!({"status": 200, "json": {"runs": [{"run_id": run, "feature": "f", "state": "running", "stage": null, "started_at": null, "updated_at": null, "target_branch": null}]}});
    let Some(out) = run("/orchestrations", json!([projects(), runs("run-a"), runs("run-b")]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(urls(&out)[..3], ["/api/projects", "/api/projects/alpha/runs", "/api/projects/beta/runs"], "{out}");
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("alpha") < at("run-a") && at("run-a") < at("beta") && at("beta") < at("run-b"), "{texts:?}");
}

#[test]
fn the_all_projects_reviews_page_lists_every_projects_reviews_under_its_name() {
    let reviews = |feature: &str| json!({"status": 200, "json": {"reviews": [{"run_id": "r-1", "feature": feature, "status": "waiting"}]}});
    let Some(out) = run("/reviews", json!([projects(), reviews("alpha-review"), reviews("beta-review")]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(urls(&out)[..3], ["/api/projects", "/api/projects/alpha/reviews", "/api/projects/beta/reviews"], "{out}");
    let texts = shown(&out["frames"][1]);
    let at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing: {texts:?}"));
    assert!(at("alpha") < at("alpha-review") && at("alpha-review") < at("beta") && at("beta") < at("beta-review"), "{texts:?}");
}

#[test]
fn a_project_that_cannot_be_read_says_so_and_the_others_still_show() {
    let ledger = json!({"status": 200, "json": {"features": [{"name": "beta-feature", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}]}});
    let Some(out) = run("/features", json!([projects(), {"status": 500, "json": {}}, ledger]), json!([{"submit": TOKEN}])) else { return };
    let texts = shown(&out["frames"][1]).join(" | ");
    assert!(texts.contains("beta-feature"), "{texts}");
    assert!(texts.contains("could not be read"), "{texts}");
}
