//! A review's risks as cards: severity badge, Fixed/Open, category, `file:line` (a link when the report
//! has a permalink) and the claim and reasoning as Markdown; open risks first by severity, then fixed ones.
//! A payload carrying only the `risks` lines keeps today's list.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 6] = ["[data-risk-card]", "[data-risk-severity]", "[data-risk-state]", "[data-risk-category]", "[data-risk-place]", ".review-section li"];

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": responses, "steps": steps, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "the harness failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).expect("the harness prints JSON"))
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [
        {"run_id": "run-1", "feature": "first-feature", "project": "p", "current": true, "status": "approved", "risk": "high", "stale": false, "link": null}
    ]}})
}

fn item(severity: &str, claim: &str, fixed: bool, file: Value, line: Value, link: Value) -> Value {
    json!({"severity": severity, "file": file, "line": line, "claim": claim, "category": "bugs", "reasoning": format!("why {claim} with `code`"), "fixed": fixed, "link": link})
}

fn detail(extra: Value) -> Value {
    let mut body = json!({
        "run_id": "run-1", "reported_head_sha": "abc1234", "observed_head_sha": "abc1234", "stale": false, "freshness": "current", "link_allowed": false, "link": null,
        "reviewed_head_sha": "1111111aaaa", "provider_head_sha": "2222222bbbb", "checked_at": "2026-11-09T10:00:00Z",
        "summary": ["adds the thing"], "risks": ["high src/a.rs:1 first"], "files": ["src/a.rs"], "tests": ["ok"], "rounds": ["round 1"]
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    json!({"status": 200, "json": body})
}

fn open(detail: Value) -> Option<Value> {
    run(json!([list(), detail]), json!([{"submit": TOKEN}, {"click": "button[data-open-run]", "index": 0}]))
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

fn position(all: &[String], needle: &str) -> usize {
    all.iter().position(|t| t.contains(needle)).unwrap_or_else(|| panic!("{needle} missing from {all:?}"))
}

#[test]
fn each_risk_is_a_card_with_its_parts_a_permalink_and_markdown() {
    let items = json!([item("high", "**Bold** claim", false, json!("src/a.rs"), json!(7), json!("https://github.com/o/r/blob/abc/src/a.rs#L7"))]);
    let Some(out) = open(detail(json!({"risk_items": items}))) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-risk-card]"), 1, "{out}");
    assert_eq!(count(frame, ".review-section li"), 4, "the card plus the other sections' three lines, and not the risk line: {out}");
    assert!(!texts(frame).iter().any(|t| t == "high src/a.rs:1 first"), "{out}");
    let shown = texts(frame);
    // The severity is a pill in words ("High"), like every other state.
    for expected in ["High", "Open", "bugs", "src/a.rs:7", "Bold", " claim", "code"] {
        assert!(shown.iter().any(|t| t.contains(expected)), "{expected}: {shown:?}");
    }
    let link = frame["settled"]["links"].as_array().unwrap().iter().find(|l| l["text"].as_str().is_some_and(|t| t.contains("src/a.rs:7"))).unwrap_or_else(|| panic!("{out}"));
    assert_eq!(link["href"], "https://github.com/o/r/blob/abc/src/a.rs#L7");
    assert!(link["rel"].as_str().unwrap().contains("noopener"), "{link}");
}

#[test]
fn a_risk_without_a_safe_permalink_or_place_shows_no_link() {
    let items = json!([
        item("medium", "unsafe link", false, json!("src/b.rs"), Value::Null, json!("javascript:alert(1)")),
        item("low", "no place", false, Value::Null, Value::Null, Value::Null),
    ]);
    let Some(out) = open(detail(json!({"risk_items": items}))) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-risk-card]"), 2, "{out}");
    assert_eq!(count(frame, "[data-risk-place]"), 1, "{out}");
    assert!(frame["settled"]["links"].as_array().unwrap().iter().all(|l| !l["href"].as_str().unwrap_or("").starts_with("javascript")), "{out}");
}

#[test]
fn open_risks_come_first_by_severity_then_the_fixed_ones() {
    let none = || (Value::Null, Value::Null, Value::Null);
    let make = |severity: &str, claim: &str, fixed: bool| {
        let (f, l, k) = none();
        item(severity, claim, fixed, f, l, k)
    };
    let items = json!([make("critical", "claim-fixed-critical", true), make("low", "claim-open-low", false), make("high", "claim-open-high", false), make("critical", "claim-open-critical", false)]);
    let Some(out) = open(detail(json!({"risk_items": items}))) else { return };
    let shown = texts(last(&out));
    let order: Vec<usize> = ["claim-open-critical", "claim-open-high", "claim-open-low", "claim-fixed-critical"].iter().map(|c| position(&shown, c)).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {shown:?}");
    assert!(shown.iter().any(|t| t == "Fixed"), "{shown:?}");
}

#[test]
fn a_payload_with_only_risk_lines_keeps_the_list() {
    let Some(out) = open(detail(json!({}))) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-risk-card]"), 0, "{out}");
    assert!(texts(frame).iter().any(|t| t == "high src/a.rs:1 first"), "{out}");
}
