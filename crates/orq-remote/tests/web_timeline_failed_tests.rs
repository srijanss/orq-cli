//! A failed Timeline step that names its failed tests keeps them behind one closed line, "Failed tests: N",
//! like the step's other details: between its "Failed: ..." line and its log lines, indented under the step
//! name and never wider than the card. Opened, it lists each test's name and output.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";
const TOGGLE: &str = "[data-step-failed-tests-toggle]";
const OPENED: &str = "[data-step-failed-tests-toggle][aria-expanded=\"true\"]";
const CLOSED: &str = "[data-step-failed-tests-toggle][aria-expanded=\"false\"]";
const LIST: &str = "[data-step-failed-tests]";
const NAMES: &str = "[data-step-failed-test-name]";
const OUTPUTS: &str = "[data-step-failed-test-output]";
const MORE: &str = "[data-step-failed-tests-more]";
const SECOND_LEVEL: &str = ".run-step details";
const BANNER_LIST: &str = "[data-failed-tests]";

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn stream() -> Value {
    json!({"status": 200, "stream": [], "end": "hold"})
}

fn run_header() -> Value {
    json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": "verify", "started_at": null, "updated_at": null})
}

/// A verify step that failed naming `tests` (and `more` unnamed ones), with a log excerpt.
fn failed_verify(tests: Value, more: Option<u64>) -> Value {
    let mut failure = json!({"reason": "step 2 exited with status 101", "log": {"file": "verify.log", "line": 42, "excerpt": "thread 'main' panicked"}, "failed_tests": tests});
    if let Some(more) = more {
        failure["failed_tests_more"] = json!(more);
    }
    json!([
        {"event_id": "e1", "kind": "stage.started", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:27:21Z"},
        {"event_id": "e2", "kind": "stage.failed", "stage": "verify", "attempt": 1, "occurred_at": "2026-11-16T15:28:21Z", "failure": failure},
    ])
}

fn page(events: Value) -> Value {
    json!({"status": 200, "json": {"run": run_header(), "events": events, "next_cursor": null}})
}

fn open_run(events: Value, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let dir = env!("CARGO_MANIFEST_DIR");
    let selectors = [TOGGLE, OPENED, CLOSED, LIST, NAMES, OUTPUTS, MORE, SECOND_LEVEL, BANNER_LIST, "[data-step-failed-tests] .field-hint"];
    let scenario = json!({"url": RUN, "responses": [page(events), projects(), stream()], "steps": all, "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not counted"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn one_test() -> Value {
    json!([{"name": "auth::tests::rejects_expired_token", "output": "assertion failed: left == right"}])
}

#[test]
fn a_failed_verify_naming_one_test_shows_a_closed_line_that_opens_to_its_name_and_output() {
    let Some(out) = open_run(failed_verify(one_test(), None), vec![json!({"click": TOGGLE})]) else { return };
    let shown = &out["frames"][1];
    assert_eq!((count(shown, TOGGLE), count(shown, CLOSED)), (1, 1), "{out}");
    assert_eq!(count(shown, NAMES), 0, "no test is named until it is opened: {out}");
    assert_eq!(count(shown, BANNER_LIST), 0, "the step has no open list of its own: {out}");
    let t = texts(shown);
    assert!(!t.iter().any(|x| x.contains("rejects_expired_token")), "{t:?}");
    let at = |needle: &str| t.iter().position(|x| x.starts_with(needle)).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("Failed: step 2") < at("Failed tests: 1"), "{t:?}");
    assert!(at("Failed tests: 1") < at("Log lines at the error"), "{t:?}");
    assert!(t.iter().any(|x| x == "Failed tests: 1"), "{t:?}");

    let opened = &out["frames"][2];
    assert_eq!((count(opened, OPENED), count(opened, NAMES), count(opened, OUTPUTS)), (1, 1, 1), "{out}");
    assert_eq!(count(opened, SECOND_LEVEL), 0, "the names are not a second disclosure: {out}");
    let t = texts(opened);
    assert!(t.iter().any(|x| x == "auth::tests::rejects_expired_token"), "{t:?}");
    assert!(t.iter().any(|x| x == "assertion failed: left == right"), "{t:?}");
}

#[test]
fn a_test_with_no_output_says_so_inside_the_detail() {
    let tests = json!([{"name": "a::kept", "output": "boom"}, {"name": "b::bare", "output": ""}]);
    let Some(out) = open_run(failed_verify(tests, None), vec![json!({"click": TOGGLE})]) else { return };
    let opened = &out["frames"][2];
    assert!(texts(&out["frames"][1]).iter().any(|x| x == "Failed tests: 2"), "{out}");
    assert_eq!((count(opened, NAMES), count(opened, OUTPUTS)), (2, 1), "{out}");
    assert_eq!(count(opened, "[data-step-failed-tests] .field-hint"), 1, "{out}");
    assert!(texts(opened).iter().any(|x| x == "No output was kept."), "{:?}", texts(opened));
}

#[test]
fn more_failed_tests_than_named_show_in_the_label_and_as_the_last_line() {
    let Some(out) = open_run(failed_verify(one_test(), Some(3)), vec![json!({"click": TOGGLE})]) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|x| x == "Failed tests: 1 (and 3 more)"), "{:?}", texts(&out["frames"][1]));
    let opened = &out["frames"][2];
    assert_eq!(count(opened, MORE), 1, "{out}");
    let t = texts(opened);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("auth::tests::rejects_expired_token") < at("and 3 more"), "{t:?}");
}

#[test]
fn a_failure_naming_no_test_has_no_such_line() {
    let Some(out) = open_run(failed_verify(json!([]), None), vec![]) else { return };
    assert_eq!(count(&out["frames"][1], TOGGLE), 0, "{out}");
}

#[test]
fn a_long_test_name_stays_indented_and_inside_the_card_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    let long = format!("crate::{}", "x".repeat(133));
    assert_eq!(long.len(), 140);
    let tests = json!([{"name": long, "output": ""}, {"name": "b::short", "output": "out"}]);
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = [web_chromium::get("/api/projects", json!({"projects": [project]})), web_chromium::get("/api/projects/alpha/runs/run-1", page(failed_verify(tests, None))["json"].clone())];
    const STEP: &str = ".run-step:has([data-step-failed-tests])";
    const NAME: &str = ".run-step:has([data-step-failed-tests]) .run-step-name";
    const CARD: &str = "#run-panel-timeline";
    for width in [390, 1440] {
        let steps = json!([{"wait": TOGGLE}, {"click": TOGGLE}, {"wait": NAMES}, {"boxes": STEP}, {"boxes": NAME}, {"boxes": CARD}, {"boxes": LIST}, {"boxes": NAMES}]);
        let result = web_chromium::measure(&chromium, width, RUN, &replies, steps).result;
        let at = format!("{width} px");
        assert!(result.get("error").is_none(), "{at}: {result}");
        assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{at}: the page scrolls sideways: {result}");
        assert_eq!(result["offenders"], json!([]), "{at}: text runs past the screen: {result}");
        let (step, name, card, list) = (&result["boxes"][STEP][0], &result["boxes"][NAME][0], &result["boxes"][CARD][0], &result["boxes"][LIST][0]);
        let edge = |b: &Value, side: &str| b[side].as_i64().unwrap_or_else(|| panic!("{at}: no {side} in {b}: {result}"));
        assert!(edge(step, "right") <= edge(card, "right"), "{at}: the step {step} ends past the card {card}");
        assert!(edge(card, "right") <= i64::from(width), "{at}: the card {card} ends past the page");
        assert!(edge(list, "right") <= edge(step, "right"), "{at}: the list {list} ends past its step {step}");
        assert!(edge(list, "left") >= edge(name, "left"), "{at}: the list {list} starts left of the step name {name}");
        for test in result["boxes"][NAMES].as_array().unwrap() {
            assert!(edge(test, "left") >= edge(name, "left") && edge(test, "right") <= edge(step, "right"), "{at}: the name {test} is outside {name}..{step}");
        }
    }
}
