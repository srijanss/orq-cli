//! A review ends without a PR/MR from the web too: the report offers Mark reviewed and Dismiss while
//! it waits, and Reopen once it was resolved or dismissed, each after a confirmation and naming the
//! head the person read; a run merged locally says so instead of linking a provider; and the list
//! shows only what is still waiting until Status: All is chosen.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MERGED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

fn ok(body: Value) -> Value {
    json!({"status": 200, "json": body})
}

fn list() -> Value {
    ok(json!({"reviews": [
        {"run_id": "run-1", "feature": "first-feature", "status": "ready", "risk": "low", "stale": false},
        {"run_id": "run-2", "feature": "merged-feature", "status": "merged", "stale": false, "merged_locally": MERGED},
        {"run_id": "run-3", "feature": "resolved-feature", "status": "resolved", "stale": false}
    ]}))
}

fn detail(status: &str, merged: Option<&str>) -> Value {
    ok(json!({"stale": false, "freshness": "current", "link": "https://github.com/o/r/pull/1", "link_allowed": true, "status": status, "merged_locally": merged, "reported_head_sha": HEAD, "reviewed_head_sha": HEAD, "summary": ["adds the thing"], "risks": [], "files": [], "tests": [], "rounds": []}))
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn a_waiting_review_is_marked_reviewed_after_a_confirmation_naming_the_head_read() {
    let responses = json!([list(), detail("ready", None), ok(json!({"run": "run-1", "status": "resolved", "head": HEAD})), list(), detail("resolved", None)]);
    let Some(out) = run("/reviews/run-1", responses, json!([{"submit": TOKEN}, {"click": "[data-review-action=resolve]"}, {"click": "[data-confirm]"}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Mark reviewed") && text.contains("Dismiss"), "{text}");
    assert!(!text.contains("Reopen"), "{text}");
    assert!(shown(&out["frames"][2]).contains("Mark this review as reviewed"), "a question comes first: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/reviews/run-1/resolve");
    assert_eq!(serde_json::from_str::<Value>(sent[0]["body"].as_str().unwrap()).unwrap(), json!({"head": HEAD}));
    assert!(shown(&out["frames"][3]).contains("Reopen"), "the report is read again: {out}");
}

#[test]
fn cancelling_the_confirmation_sends_nothing() {
    let Some(out) = run("/reviews/run-1", json!([list(), detail("ready", None)]), json!([{"submit": TOKEN}, {"click": "[data-review-action=dismiss]"}, {"click": "[data-cancel]"}])) else { return };
    assert!(shown(&out["frames"][2]).contains("Dismiss this review"), "{out}");
    assert!(posts(&out).is_empty(), "{out}");
}

#[test]
fn a_resolved_review_is_reopened_and_a_newer_report_is_shown_as_the_reason_nothing_changed() {
    let responses = json!([list(), detail("resolved", None), {"status": 409, "json": {"error": "run run-1 has a newer report", "latest": MERGED}}]);
    let Some(out) = run("/reviews/run-1", responses, json!([{"submit": TOKEN}, {"click": "[data-review-action=reopen]"}, {"click": "[data-confirm]"}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Reopen") && !text.contains("Mark reviewed"), "{text}");
    assert_eq!(posts(&out)[0]["url"], "/api/reviews/run-1/reopen");
    assert!(shown(&out["frames"][3]).contains("has a newer report"), "{out}");
}

#[test]
fn a_run_merged_locally_names_its_commit_and_offers_no_provider_link_or_action() {
    let Some(out) = run("/reviews/run-2", json!([list(), detail("merged", Some(MERGED))]), json!([{"submit": TOKEN}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Merged locally at") && text.contains("| bbbbbbb |"), "{text}");
    assert!(!text.contains("GitHub") && !text.contains("Mark reviewed") && !text.contains("Reopen"), "{text}");
}

#[test]
fn the_list_shows_what_is_waiting_until_all_statuses_are_chosen() {
    let Some(out) = run("/reviews", json!([list()]), json!([{"submit": TOKEN}, {"set": "select[data-filter=status]", "value": ""}])) else { return };
    let waiting = shown(&out["frames"][1]);
    assert!(waiting.contains("first-feature") && !waiting.contains("merged-feature") && !waiting.contains("resolved-feature"), "{waiting}");
    let all = shown(&out["frames"][2]);
    assert!(all.contains("merged-feature") && all.contains("resolved-feature"), "{all}");
}
