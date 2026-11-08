//! The React page behaves like the page it replaces: the built bundle runs in jsdom against scripted replies.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const STALE_MARKER: &str = "stale, may be out of date";

/// The harness output, or None when node or the web-src dependencies are not installed.
fn run(responses: Value, steps: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    if !web_deps::ready() {
        return None;
    }
    let scenario = json!({"responses": responses, "steps": steps});
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

fn page(out: &Value, frame: usize) -> String {
    out["frames"][frame]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn review(id: &str, feature: &str, status: &str, stale: bool) -> Value {
    json!({"run_id": id, "feature": feature, "project": "p", "current": true, "status": status, "risk": "low", "stale": stale, "link": null})
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [review("run-1", "first-feature", "approved", false), review("run-2", "second-feature", "needs changes", true)]}})
}

fn detail(stale: bool, link: Value) -> Value {
    json!({"status": 200, "json": {"stale": stale, "link": link, "summary": ["adds the thing"], "risks": [], "files": ["a.rs"], "tests": ["t"], "rounds": []}})
}

#[test]
fn the_token_is_asked_for_then_the_list_is_read_with_it_and_the_field_is_emptied() {
    let Some(out) = run(json!([list()]), json!([{"submit": TOKEN}])) else { return };
    let start = &out["frames"][0]["settled"];
    assert_eq!(start["inputs"][0]["type"], "password");
    assert_eq!(start["inputs"][0]["autocomplete"], "off");
    assert_eq!(out["frames"][1]["fieldAfter"], "");
    assert_eq!(out["requests"][0]["url"], "/api/reviews");
    assert_eq!(out["requests"][0]["method"], "GET");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let shown = page(&out, 1);
    assert!(shown.find("first-feature").unwrap() < shown.find("second-feature").unwrap(), "{shown}");
    for frame in out["frames"].as_array().unwrap() {
        assert!(!frame["settled"].to_string().contains(TOKEN) && !frame["sync"].to_string().contains(TOKEN), "the token leaked into the page");
    }
}

#[test]
fn only_a_stale_report_carries_the_stale_marker() {
    let Some(out) = run(json!([list()]), json!([{"submit": TOKEN}])) else { return };
    let marked = out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter(|t| t.as_str().unwrap().contains(STALE_MARKER)).count();
    assert_eq!(marked, 1, "{out}");
}

#[test]
fn text_from_the_api_stays_text() {
    let nasty = "<img src=x onerror=alert(1)>";
    let reply = json!({"status": 200, "json": {"reviews": [review("r1", nasty, "<b>ok</b>", false)]}});
    let Some(out) = run(json!([reply]), json!([{"submit": TOKEN}])) else { return };
    let shown = page(&out, 1);
    assert!(shown.contains(nasty) && shown.contains("<b>ok</b>"), "{shown}");
}

#[test]
fn a_401_asks_for_the_token_again_with_one_message() {
    for reply in [json!({"status": 401})] {
        let Some(out) = run(json!([reply]), json!([{"submit": TOKEN}])) else { return };
        let settled = &out["frames"][1]["settled"];
        assert_eq!(settled["inputs"].as_array().unwrap().len(), 1, "{out}");
        assert!(page(&out, 1).contains("Could not sign in. Check the token and try again."), "{out}");
        assert!(!settled.to_string().contains(TOKEN));
    }
}

#[test]
fn a_report_opens_with_its_sections_and_a_safe_link_and_back_returns_to_the_list() {
    let Some(out) = run(json!([list(), detail(false, json!("https://github.com/o/r/pull/7")), list()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"back": true}])) else { return };
    assert_eq!(out["requests"][1]["url"], "/api/reviews/run-1");
    let shown = page(&out, 2);
    assert!(shown.contains("adds the thing") && shown.contains("none recorded") && shown.contains("Review rounds"), "{shown}");
    let links = out["frames"][2]["settled"]["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{out}");
    assert_eq!(links[0]["href"], "https://github.com/o/r/pull/7");
    assert_eq!(links[0]["rel"], "noopener noreferrer");
    let back = page(&out, 3);
    assert!(back.contains("first-feature") && back.contains("second-feature"), "{back}");
    assert_eq!(out["frames"][3]["settled"]["hash"], "");
}

#[test]
fn a_stale_report_shows_the_marker_and_no_link_and_an_unsafe_link_is_never_offered() {
    let Some(out) = run(json!([list(), detail(true, json!("https://github.com/o/r/pull/7"))]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    assert!(page(&out, 2).contains(STALE_MARKER), "{out}");
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
    for link in ["http://x.test/a", "javascript:alert(1)", "https://user:pw@x.test/a"] {
        let Some(out) = run(json!([list(), detail(false, json!(link))]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
        assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{link}: {out}");
    }
}

#[test]
fn a_failed_detail_keeps_the_token_and_offers_a_retry_that_asks_again() {
    for first in [json!({"status": 503}), json!({"status": 200, "json": {"summary": "nope"}})] {
        let Some(out) = run(json!([list(), first, detail(false, Value::Null)]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"click": "button", "index": 0}])) else { return };
        assert!(out["frames"][2]["settled"]["inputs"].as_array().unwrap().is_empty(), "{out}");
        let requests = out["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 3, "{out}");
        assert_eq!(requests[2]["url"], "/api/reviews/run-1");
        assert_eq!(requests[2]["headers"], requests[0]["headers"]);
        assert!(page(&out, 3).contains("adds the thing"), "{out}");
    }
}

#[test]
fn a_slow_answer_for_an_earlier_page_never_draws_over_the_current_one() {
    let slow = json!({"status": 200, "hold": "json", "json": detail(false, Value::Null)["json"].clone()});
    let Some(out) = run(json!([list(), slow, list()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"back": true}, {"release": 1}])) else { return };
    let shown = page(&out, 4);
    assert!(shown.contains("first-feature") && !shown.contains("adds the thing"), "{shown}");
}
