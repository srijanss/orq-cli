//! While an accept runs, the run reads "Accepting" with a clock, its page says the tests are running and since when,
//! and none of Accept, Review again, Resume or Discard is offered: the reason says an accept is running. A failed
//! accept shows why with the output; a passing one says what next, by how the run delivers.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = ["[data-accept]", "[data-resume]", "[data-discard]", "[data-accepting]", "[data-accept-busy]", "[data-accept-failure]", "[data-accept-output]", "[data-accepted]", ".run-accepting"];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn one(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"})
}

fn detail(run: Value) -> Option<Value> {
    let id = run["run_id"].as_str().unwrap().to_string();
    let body = json!({"run": run, "events": [], "next_cursor": null, "remote_actions": true});
    // An empty run follows its events live; the follow gets its own held stream.
    let responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    self::run(json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS}))
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

fn minutes_ago(minutes: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - minutes * 60;
    let (days, secs) = (now.div_euclid(86_400), now.rem_euclid(86_400));
    // Civil date from days since the epoch (Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

#[test]
fn an_accepting_run_says_so_with_since_when_and_offers_none_of_the_actions_saying_why() {
    let mut accepting = one("r-acc", "accepting");
    accepting["accepting_since"] = json!(minutes_ago(3));
    let Some(out) = detail(accepting) else { return };
    let end = last(&out);
    let all = texts(end);
    assert!(all.iter().any(|t| t == "Accepting"), "the state badge: {all:?}");
    assert_eq!(count(end, ".run-accepting"), 1, "{out}");
    assert_eq!(count(end, "[data-accepting]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "Running the tests in the worktree (started )"), "{all:?}");
    assert!(all.iter().any(|t| t.contains("3 min")), "since when: {all:?}");
    for action in ["[data-accept]", "[data-resume]", "[data-discard]"] {
        assert_eq!(count(end, action), 0, "{action} is not offered while accepting: {out}");
    }
    assert_eq!(count(end, "[data-accept-busy]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "Accept, Review again, Resume and Discard wait: an accept is running"), "{all:?}");
}

#[test]
fn the_run_list_shows_the_accepting_badge() {
    let rows = json!({"status": 200, "json": {"runs": [one("r-acc", "accepting")], "next_cursor": null, "total": 1, "running": 0, "remote_actions": true}});
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations", "responses": [rows, projects()], "steps": [{"submit": TOKEN}], "selectors": SELECTORS})) else { return };
    let end = last(&out);
    assert_eq!(count(end, ".run-accepting"), 1, "{out}");
    assert!(texts(end).iter().any(|t| t == "Accepting"), "{out}");
}

#[test]
fn a_failed_accept_shows_why_with_the_output_and_the_run_can_be_fixed_or_discarded() {
    let mut failed = one("r-rev", "needs_review");
    failed["accept_failure"] = json!({"reason": "the tests failed in r-rev: `cargo test` failed: test result: FAILED. 1 failed", "output": "test a ... FAILED\ntest result: FAILED. 1 failed", "at": minutes_ago(1)});
    let Some(out) = detail(failed) else { return };
    let end = last(&out);
    let all = texts(end);
    assert_eq!(count(end, "[data-accept-failure]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "Accept failed: the tests failed in r-rev: `cargo test` failed: test result: FAILED. 1 failed"), "{all:?}");
    assert_eq!(count(end, "[data-accept-output]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "test a ... FAILED\ntest result: FAILED. 1 failed"), "{all:?}");
    assert_eq!(count(end, "[data-accept]"), 1, "accept can be tried again: {out}");
    assert_eq!(count(end, "[data-discard]"), 1, "{out}");
}

#[test]
fn a_passing_accept_says_what_next_by_how_the_run_delivers() {
    let mut merged = one("r-ok", "completed");
    merged["accepted"] = json!(true);
    merged["delivery"] = json!("merge_local");
    let Some(out) = detail(merged) else { return };
    let all = texts(last(&out));
    assert_eq!(count(last(&out), "[data-accepted]"), 1, "{out}");
    assert!(all.iter().any(|t| t == "Tests passed. Merge into main?"), "{all:?}");

    let mut review = one("r-ok", "completed");
    review["accepted"] = json!(true);
    review["delivery"] = json!("review");
    let Some(out) = detail(review) else { return };
    let all = texts(last(&out));
    assert!(all.iter().any(|t| t == "Tests passed. The branch is ready to push and open a review."), "{all:?}");

    let Some(out) = detail(one("r-plain", "completed")) else { return };
    assert_eq!(count(last(&out), "[data-accepted]"), 0, "a run that was not accepted says nothing of it: {out}");
}
