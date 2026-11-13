//! Text written by agents, commits and logs is untrusted: every page draws it literally and
//! makes no element, and a provider URL that is not plain https without credentials is no link.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const HOSTILE: &str = "<img src=x onerror=alert(1)><script>alert(2)</script><b>bold</b>";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

fn literal_and_inert(out: &Value, frame: usize, surface: &str) {
    let settled = &out["frames"][frame]["settled"];
    let texts: Vec<&str> = settled["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains(HOSTILE)), "{surface} does not show the text literally: {texts:?}");
    assert_eq!(settled["injected"], 0, "{surface} made an element from text: {out}");
}

#[test]
fn feature_text_is_shown_literally_and_makes_no_element() {
    let ledger = json!({"status": 200, "json": {"features": [{"name": "f1", "description": HOSTILE, "status": "pending", "depends_on": [], "recorded_at": null}]}});
    let Some(out) = run("/projects/alpha/features/f1", json!([ledger, {"status": 200, "json": {"runs": []}}, projects()]), json!([{"submit": TOKEN}])) else { return };
    literal_and_inert(&out, 1, "the feature page");
}

#[test]
fn log_lines_are_shown_literally_and_make_no_element() {
    let list = json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}]}});
    let page = json!({"status": 200, "json": {"name": "implement.log", "lines": [HOSTILE], "next_cursor": null, "truncated": false}});
    let run_page = json!({"status": 200, "json": {"run": {"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": null, "started_at": null, "updated_at": null}, "events": [], "next_cursor": null}});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run("/projects/alpha/orchestrations/run-1/logs/implement.log", json!([run_page, projects(), stream, list, page]), json!([{"submit": TOKEN}])) else { return };
    literal_and_inert(&out, 1, "a log page");
}

#[test]
fn a_review_report_is_literal_and_only_plain_https_links_are_offered() {
    let list = json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": HOSTILE, "project": "p", "current": true, "status": "approved", "risk": "low", "stale": false, "link": null}]}});
    let detail_with = |link: &str, allowed: bool| json!({"status": 200, "json": {
        "run_id": "run-1", "reported_head_sha": "abc1234", "observed_head_sha": "abc1234", "stale": false, "freshness": "current", "link_allowed": allowed, "link": link,
        "reviewed_head_sha": "1111111aaaa", "provider_head_sha": "2222222bbbb", "checked_at": "2026-11-09T10:00:00Z",
        "summary": [HOSTILE], "risks": [], "files": [], "tests": [], "rounds": []
    }});
    let detail = |link: &str| detail_with(link, true);
    let steps = json!([{"submit": TOKEN}, {"click": "button[data-open-run]", "index": 0}]);
    let Some(out) = run("/reviews", json!([list.clone(), detail("https://github.com/o/r/pull/7")]), steps.clone()) else { return };
    literal_and_inert(&out, 1, "the review inbox");
    literal_and_inert(&out, 2, "a review report");
    let links = report_links(&out["frames"][2]);
    assert_eq!(links.len(), 1, "{out}");
    assert_eq!(links[0]["href"], "https://github.com/o/r/pull/7", "{out}");
    let Some(out) = run("/reviews", json!([list.clone(), detail_with("https://github.com/o/r/pull/7", false)]), steps.clone()) else { return };
    assert!(report_links(&out["frames"][2]).is_empty(), "a link the server does not allow: {out}");
    for bad in ["http://github.com/o/r/pull/7", "https://user:pw@github.com/x", "javascript:alert(1)"] {
        let Some(out) = run("/reviews", json!([list.clone(), detail(bad)]), steps.clone()) else { return };
        assert!(report_links(&out["frames"][2]).is_empty(), "{bad}: {out}");
    }
}

/// The links the report offers: every link on the page but the settings cog beside the project picker.
fn report_links(frame: &Value) -> Vec<&Value> {
    frame["settled"]["links"].as_array().unwrap().iter().filter(|l| !l["href"].as_str().unwrap_or("").ends_with("/settings")).collect()
}
