//! On a phone the features list has no marks and no "promote/start the marked ones" buttons: a row is swiped
//! left to show Promote (a draft) or Start (a pending feature) for that one feature, inline in the row beside
//! its state, and a swipe right hides it again. The row does not slide away. The revealed button asks first,
//! as everywhere, then sends that feature's own promote or start. On a wider screen the marked-actions
//! buttons sit apart with a gap.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = [
    "tr[data-feature-row=\"d1\"].swiped > td > .feature-state-cell > button[data-swipe-promote=\"d1\"]",
    "tr[data-feature-row=\"p1\"].swiped > td > .feature-state-cell > button[data-swipe-start=\"p1\"]",
    "[data-swipe-promote], [data-swipe-start]",
    "tr.feature.swiped",
    "tr.swipe-confirm .confirm [data-confirm]",
    "td.swipe-cell, th.swipe-cell",
    "tr.feature.swiped > td > .feature-state-cell > .feature-state",
];

fn ledger() -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("d1", "draft"), feature("p1", "pending"), feature("c1", "completed")], "remote_actions": true}})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run(steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let started = json!({"status": 200, "json": {"run": "run-7"}});
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger(), projects(), started, ledger(), projects()], "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn a_swipe_left_shows_the_rows_one_action_inline_beside_its_state_and_a_swipe_right_hides_it() {
    let steps = vec![
        json!({"swipe": "[data-feature-row=\"p1\"]", "dx": -90}),
        json!({"swipe": "[data-feature-row=\"d1\"]", "dx": -90}),
        json!({"swipe": "[data-feature-row=\"d1\"]", "dx": 90}),
        json!({"swipe": "[data-feature-row=\"c1\"]", "dx": -90}),
    ];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "nothing until a swipe: {out}");
    assert_eq!(count(&frames[1], SELECTORS[5]), 0, "no column of its own: {out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "a pending feature offers Start: {out}");
    assert_eq!(count(&frames[2], SELECTORS[6]), 1, "beside its state: {out}");
    assert_eq!(count(&frames[3], SELECTORS[0]), 1, "a draft offers Promote: {out}");
    assert_eq!(count(&frames[3], SELECTORS[3]), 1, "one row at a time: {out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 0, "{out}");
    assert_eq!(count(&frames[5], SELECTORS[3]), 0, "a completed one offers nothing: {out}");
}

#[test]
fn the_revealed_start_asks_then_starts_that_one_feature() {
    let steps = vec![json!({"swipe": "[data-feature-row=\"p1\"]", "dx": -90}), json!({"click": "[data-swipe-start=\"p1\"]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[3], SELECTORS[4]), 1, "{out}");
    let texts: Vec<&str> = frames[3]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Start p1?"), "{texts:?}");
    let posts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/features/p1/runs");
}

#[test]
fn a_phone_hides_the_marks_and_the_swiped_row_stays_in_place_and_a_wide_screen_spaces_the_marked_buttons() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    assert!(!css.contains("translateX(-7.5rem)"), "the row does not slide away");
    let spaced = css.lines().find(|line| line.starts_with(".marked-actions {")).expect("a .marked-actions rule");
    assert!(spaced.contains("gap:") && spaced.contains("margin"), "{spaced}");
    assert!(css.contains("  .feature-mark, .marked-actions { display: none; }"), "a phone has no marks");
    let cell = css.lines().find(|line| line.starts_with(".feature-state-cell {")).expect("a .feature-state-cell rule");
    assert!(cell.contains("display: inline-flex") && cell.contains("gap:"), "{cell}");
    // The name column's 10rem floor would push the revealed button off a phone's screen.
    assert!(css.contains("  .feature-table:has(tr.feature.swiped) :is(th, td):first-child { min-width: 0; }"), "a swiped list lets its names shrink");
}
