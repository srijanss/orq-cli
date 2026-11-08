//! The page keeps its place in the address path, so reload, Back and Forward restore it.

mod web_deps;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn run(scenario: Value) -> Option<Value> {
    web_deps::ready().then(|| {
        let output = Command::new("node").arg(crate_dir().join("tests/web_react_harness.mjs")).arg(crate_dir().join("web/app.react.js")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

fn texts(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect::<Vec<_>>().join(" | ")
}

fn inbox() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "alpha", "status": "approved", "stale": false}]}})
}

fn report() -> Value {
    json!({"status": 200, "json": {"stale": false, "summary": ["the summary line"], "risks": [], "files": [], "tests": [], "rounds": []}})
}

#[test]
fn opening_a_report_changes_the_path_and_back_returns_to_the_inbox() {
    let Some(out) = run(json!({"responses": [inbox(), report(), inbox()], "steps": [{"submit": "token"}, {"click": "button", "index": 0}, {"back": true}]})) else { return };
    assert_eq!(out["frames"][1]["settled"]["path"], "/", "{out}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/reviews/run-1", "{out}");
    assert!(texts(&out["frames"][2]).contains("the summary line"), "{out}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/", "{out}");
    assert!(texts(&out["frames"][3]).contains("alpha"), "{out}");
}

#[test]
fn a_report_address_opens_that_report_directly() {
    let Some(out) = run(json!({"url": "/reviews/run-1", "responses": [report()], "steps": [{"submit": "token"}]})) else { return };
    assert!(texts(&out["frames"][1]).contains("the summary line"), "{out}");
    assert_eq!(out["requests"][0]["url"], "/api/reviews/run-1", "{out}");
}

#[test]
fn following_a_sidebar_link_changes_the_path_without_a_reload_and_back_undoes_it() {
    let Some(out) = run(json!({"responses": [inbox(), inbox()], "steps": [{"submit": "token"}, {"nav": "nav a[href='/settings']"}, {"back": true}]})) else { return };
    let settings = &out["frames"][2]["settled"];
    assert_eq!(settings["path"], "/settings", "{out}");
    assert!(texts(&out["frames"][2]).contains("not available yet"), "{out}");
    let back = &out["frames"][3]["settled"];
    assert_eq!(back["path"], "/", "{out}");
    assert!(texts(&out["frames"][3]).contains("alpha"), "{out}");
}

#[test]
fn a_modified_click_on_a_sidebar_link_is_left_to_the_browser() {
    let Some(out) = run(json!({"responses": [inbox()], "steps": [{"submit": "token"}, {"nav_modified": "nav a[href='/settings']"}]})) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/", "a ctrl-click must not navigate this tab: {out}");
}
