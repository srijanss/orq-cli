//! The run list the page draws from the inbox list endpoint, checked by running
//! the page script under node against scripted replies.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const STALE_MARKER: &str = "stale, may be out of date";

/// The harness output, or None when node is not installed.
fn run(reviews: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": [{"status": 200, "json": {"reviews": reviews}}], "steps": [{"submit": TOKEN}]});
    if !web_deps::ready() {
        return None;
    }
    match Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output() {
        Ok(output) => {
            assert!(output.status.success(), "the harness failed: {}", String::from_utf8_lossy(&output.stderr));
            Some(serde_json::from_slice(&output.stdout).expect("the harness prints JSON"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: node is not installed");
            None
        }
        Err(error) => panic!("cannot run node: {error}"),
    }
}

fn texts(out: &Value) -> Vec<String> {
    out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str().map(str::to_string)).collect()
}

fn review(feature: &str, status: &str, stale: bool) -> Value {
    json!({"run_id": "r1", "feature": feature, "project": "p", "current": true, "status": status, "risk": "low", "stale": stale, "link": null})
}

#[test]
fn each_run_shows_its_feature_and_status_in_the_order_given() {
    let Some(out) = run(json!([review("first-feature", "approved", false), review("second-feature", "needs changes", false)])) else { return };
    let shown = texts(&out).join(" | ");
    let first = shown.find("first-feature").expect(&shown);
    let second = shown.find("second-feature").expect(&shown);
    assert!(first < second, "{shown}");
    assert!(shown.contains("approved") && shown.contains("needs changes"), "{shown}");
}

#[test]
fn only_a_stale_report_carries_the_stale_marker() {
    let Some(out) = run(json!([review("old-one", "approved", true), review("fresh-one", "approved", false)])) else { return };
    let marked = texts(&out).iter().filter(|t| t.contains(STALE_MARKER)).count();
    assert_eq!(marked, 1, "{out}");
}

#[test]
fn text_from_the_api_stays_text() {
    let nasty = "<img src=x onerror=alert(1)>";
    let Some(out) = run(json!([review(nasty, "<b>ok</b>", false)])) else { return };
    let shown = texts(&out).join(" | ");
    assert!(shown.contains(nasty) && shown.contains("<b>ok</b>"), "{shown}");
}

#[test]
fn an_empty_inbox_says_so() {
    let Some(out) = run(json!([])) else { return };
    assert!(texts(&out).join(" ").to_lowercase().contains("no reviews"), "{out}");
}

#[test]
fn the_page_is_sized_for_a_phone() {
    let html = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/index.html")).unwrap();
    assert!(html.contains("name=\"viewport\"") && html.contains("width=device-width"));
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    assert!(css.contains("min-height: 44px"), "tap targets are at least 44px");
}

#[test]
fn a_malformed_inbox_keeps_the_token_and_offers_a_retry_instead_of_asking_to_sign_in() {
    let good = review("fine", "approved", false);
    for bad in [json!(null), json!("text"), json!({"status": "approved"}), json!({"feature": "no-status"}), json!({"feature": 7, "status": "x"})] {
        let Some(out) = run(json!([good.clone(), bad.clone()])) else { return };
        let settled = &out["frames"][1]["settled"];
        assert!(settled["inputs"].as_array().unwrap().is_empty(), "{bad}: the token field must not come back: {out}");
        assert_eq!(settled["buttons"].as_array().unwrap().iter().filter(|b| *b != "All projects").count(), 1, "{bad}: a retry button is offered: {out}");
        let shown = texts(&out).join(" | ");
        assert!(!shown.contains("undefined") && !shown.contains("Could not sign in"), "{bad}: {shown}");
    }
}

#[test]
fn a_reply_that_is_not_json_keeps_the_token_too() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": [{"status": 200, "badjson": true}], "steps": [{"submit": TOKEN}]});
    if !web_deps::ready() {
        return;
    }
    let Ok(output) = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output() else { return };
    assert!(output.status.success());
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().is_empty(), "{out}");
    assert_eq!(out["frames"][1]["settled"]["buttons"].as_array().unwrap().iter().filter(|b| *b != "All projects").count(), 1, "{out}");
}
