//! Opening one run from the list: the detail the page draws from the detail
//! endpoint, its link, the stale marker and the Back route, checked by running
//! the page script under node against scripted replies.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const STALE_MARKER: &str = "stale, may be out of date";

fn run(responses: Value, steps: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": responses, "steps": steps});
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

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [
        {"run_id": "run-1", "feature": "first-feature", "project": "p", "current": true, "status": "approved", "risk": "low", "stale": false, "link": null},
        {"run_id": "run-2", "feature": "second-feature", "project": "p", "current": true, "status": "approved", "risk": "low", "stale": false, "link": null}
    ]}})
}

fn detail(stale: bool, link: Value) -> Value {
    let freshness = if stale { "stale" } else { "current" };
    detail_with(freshness, !stale && !link.is_null(), link)
}

fn detail_with(freshness: &str, link_allowed: bool, link: Value) -> Value {
    json!({"status": 200, "json": {
        "run_id": "run-1", "reported_head_sha": "abc1234", "observed_head_sha": "abc1234", "stale": freshness == "stale", "freshness": freshness, "link_allowed": link_allowed, "link": link,
        "reviewed_head_sha": "1111111aaaa", "provider_head_sha": if freshness == "unverified" { Value::Null } else { json!("2222222bbbb") }, "checked_at": "2026-11-09T10:00:00Z",
        "summary": ["adds the thing"], "risks": ["<b>risky</b> change"], "files": ["src/a.rs"],
        "tests": ["12 passed, 0 failed"], "rounds": ["round 1: 1 finding open <img src=x onerror=alert(1)>"]
    }})
}

fn open_first(detail: Value) -> Option<Value> {
    run(json!([list(), detail]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}]))
}

fn page(out: &Value, frame: usize) -> String {
    out["frames"][frame]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn opening_a_run_reads_its_report_with_the_held_token_and_shows_every_section_as_text() {
    let Some(out) = open_first(detail(false, Value::Null)) else { return };
    let requests = out["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "{out}");
    assert_eq!(requests[1]["url"], "/api/reviews/run-1");
    assert_eq!(requests[1]["method"], "GET");
    assert_eq!(requests[1]["headers"], requests[0]["headers"]);
    let shown = page(&out, 2);
    for expected in ["adds the thing", "<b>risky</b> change", "src/a.rs", "12 passed, 0 failed", "<img src=x onerror=alert(1)>"] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    assert!(!shown.contains(STALE_MARKER), "{shown}");
    assert!(!shown.contains(TOKEN));
}

#[test]
fn only_a_safe_https_link_is_offered_and_it_opens_without_opener_or_referrer() {
    let Some(out) = open_first(detail(false, json!("https://github.com/o/r/pull/7"))) else { return };
    let links = out["frames"][2]["settled"]["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{out}");
    assert_eq!(links[0]["href"], "https://github.com/o/r/pull/7");
    let rel = links[0]["rel"].as_str().unwrap();
    assert!(rel.contains("noopener") && rel.contains("noreferrer"), "{rel}");

    for bad in ["javascript:alert(1)", "http://github.com/o/r/pull/7", "data:text/html,x", "//evil.test/x", "https://user:pw@github.com/x", "not a url", ""] {
        let Some(out) = open_first(detail(false, json!(bad))) else { return };
        assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{bad:?}: {out}");
    }
}

#[test]
fn a_stale_report_shows_the_marker_and_no_link_even_when_the_link_is_safe() {
    let Some(out) = open_first(detail(true, json!("https://github.com/o/r/pull/7"))) else { return };
    assert!(page(&out, 2).contains("STALE"), "{out}");
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
}

const LINK: &str = "https://github.com/o/r/pull/7";

#[test]
fn a_stale_report_shows_a_red_banner_with_both_hashes_above_the_summary_and_no_link() {
    let Some(out) = open_first(detail_with("stale", false, json!(LINK))) else { return };
    let shown = page(&out, 2);
    for expected in ["STALE", "1111111aaaa", "2222222bbbb"] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    assert!(shown.find("STALE").unwrap() < shown.find("adds the thing").unwrap(), "{shown}");
    assert!(!shown.contains("UNVERIFIED"), "{shown}");
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn an_unverified_report_shows_an_amber_banner_and_no_link_and_names_no_provider_head() {
    let Some(out) = open_first(detail_with("unverified", false, json!(LINK))) else { return };
    let shown = page(&out, 2);
    assert!(shown.contains("UNVERIFIED") && shown.contains("1111111aaaa"), "{shown}");
    assert!(shown.find("UNVERIFIED").unwrap() < shown.find("adds the thing").unwrap(), "{shown}");
    assert!(!shown.contains("STALE") && !shown.contains("2222222bbbb"), "{shown}");
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn a_current_report_has_no_banner_and_the_link_needs_the_server_to_allow_it() {
    let Some(out) = open_first(detail_with("current", true, json!(LINK))) else { return };
    let shown = page(&out, 2);
    assert!(!shown.contains("STALE") && !shown.contains("UNVERIFIED"), "{shown}");
    assert_eq!(out["frames"][2]["settled"]["links"].as_array().unwrap().len(), 1, "{out}");
    // A current report whose link the server does not allow, or one with no freshness at all, gets no link.
    let Some(out) = open_first(detail_with("current", false, json!(LINK))) else { return };
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
    let mut old = detail_with("current", true, json!(LINK));
    old["json"].as_object_mut().unwrap().remove("freshness");
    let Some(out) = open_first(old) else { return };
    let shown = page(&out, 2);
    assert!(shown.contains("UNVERIFIED"), "a report without a freshness is unverified: {shown}");
    assert!(out["frames"][2]["settled"]["links"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn back_returns_to_the_list() {
    let Some(out) = run(json!([list(), detail(false, Value::Null), list()]), json!([{"submit": TOKEN}, {"click": "button", "index": 1}, {"back": true}])) else { return };
    assert_eq!(out["requests"][1]["url"], "/api/reviews/run-2");
    let shown = page(&out, 3);
    assert!(shown.contains("first-feature") && shown.contains("second-feature"), "{shown}");
    assert_eq!(out["frames"][3]["settled"]["hash"], "");
}

#[test]
fn a_failed_detail_keeps_the_token_and_offers_a_retry_that_asks_again() {
    for first in [json!({"status": 503}), json!({"status": 200, "json": {"summary": "nope"}})] {
        let Some(out) = run(json!([list(), first, detail(false, Value::Null)]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}, {"click": "button", "index": 0}])) else { return };
        let failed = &out["frames"][2]["settled"];
        assert!(failed["inputs"].as_array().unwrap().is_empty(), "{out}");
        let requests = out["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 3, "{out}");
        assert_eq!(requests[2]["url"], "/api/reviews/run-1");
        assert_eq!(requests[2]["headers"], requests[0]["headers"]);
        assert!(page(&out, 3).contains("adds the thing"), "{out}");
    }
}

#[test]
fn a_401_on_the_detail_asks_for_the_token_again() {
    let Some(out) = run(json!([list(), {"status": 401}]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    let settled = &out["frames"][2]["settled"];
    assert_eq!(settled["inputs"].as_array().unwrap().len(), 1, "{out}");
    assert!(!settled.to_string().contains(TOKEN));
}

#[test]
fn the_page_sends_only_gets_and_the_one_confirmed_action_post() {
    let code = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap();
    let code: String = code.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for banned in ["\"PUT\"", "\"DELETE\"", "\"PATCH\"", "FormData", "<form"] {
        assert!(!code.contains(banned), "{banned}");
    }
    // The one POST is the action helper's, which every Start, Pause and Resume goes through.
    assert_eq!(code.matches("\"POST\"").count(), 1, "a single POST site");
    assert!(code.contains("function actionRequest(") && code.find("\"POST\"") > code.find("function actionRequest("), "the POST belongs to actionRequest");
    assert!(code.contains("encodeURIComponent"), "the run id is encoded into the path");
}
