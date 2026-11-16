//! The review list is paged ten at a time with the same pager as the Features and Orchestrations
//! lists; a filter, range or sort change starts again on the first page.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/reviews", "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// `count` reviews, f-01 the newest; the odd ones are approved, the even ones need changes.
fn reviews(count: u32) -> Value {
    let rows: Vec<Value> = (1..=count)
        .map(|n| {
            let status = if n % 2 == 1 { "approved" } else { "needs changes" };
            json!({"run_id": format!("run-{n}"), "feature": format!("f-{n:02}"), "status": status, "risk": "low", "stale": false, "freshness": "current", "generated_at": format!("2026-10-{:02}T10:00:00Z", 31 - n)})
        })
        .collect();
    json!({"status": 200, "json": {"reviews": rows}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

/// The feature names on the rows, in the order shown.
fn rows(frame: &Value) -> Vec<String> {
    texts(frame).into_iter().filter(|t| t.starts_with("f-")).collect()
}

#[test]
fn a_long_review_list_is_paged_ten_at_a_time() {
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-page-next]"}, {"nav": "[data-page-next]"}, {"nav": "[data-page-prev]"}]);
    let Some(out) = run(json!([reviews(23)]), steps) else { return };
    let first = rows(&out["frames"][1]);
    assert_eq!((first.len(), first[0].as_str(), first[9].as_str()), (10, "f-01", "f-10"), "{out}");
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "Page 1 of 3"), "{out}");
    let second = rows(&out["frames"][2]);
    assert_eq!((second.len(), second[0].as_str()), (10, "f-11"), "{out}");
    let last = rows(&out["frames"][3]);
    assert_eq!((last.len(), last[0].as_str()), (3, "f-21"), "the last page is the remainder: {out}");
    assert_eq!(rows(&out["frames"][4])[0], "f-11", "{out}");
}

#[test]
fn a_short_review_list_has_no_pager() {
    let Some(out) = run(json!([reviews(10)]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(rows(&out["frames"][1]).len(), 10, "{out}");
    assert!(!texts(&out["frames"][1]).iter().any(|t| t.starts_with("Page ")), "{out}");
}

#[test]
fn a_filter_a_range_or_a_sort_change_returns_to_the_first_page() {
    let next = json!({"nav": "[data-page-next]"});
    let steps = json!([{"submit": TOKEN}, next, {"set": "select[data-filter=status]", "value": "approved"}, next, {"nav": "[data-review-sort]"}, next, {"nav": "[data-review-range=today]"}, {"nav": "[data-review-range=all]"}]);
    let Some(out) = run(json!([reviews(23)]), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(rows(&frames[2])[0], "f-11", "{out}");
    let approved = rows(&frames[3]);
    assert_eq!((approved.len(), approved[0].as_str()), (10, "f-01"), "a filter starts on page 1: {out}");
    assert_eq!(rows(&frames[4])[0], "f-21", "{out}");
    assert_eq!(rows(&frames[5])[0], "f-23", "a sort change starts on page 1, oldest first: {out}");
    assert_eq!(rows(&frames[6])[0], "f-03", "{out}");
    assert_eq!(rows(&frames[8])[0], "f-23", "a range change starts on page 1: {out}");
}
