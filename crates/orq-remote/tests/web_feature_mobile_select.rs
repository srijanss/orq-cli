//! On a phone the features list can select several features: a Select button (or a long press on a row) enters
//! selection mode, where each draft or pending row shows its mark and a tap on the row toggles it instead of
//! opening the feature. While anything is marked a bar fixed at the bottom says how many are selected and holds
//! the same Promote N and Start N as a wide screen. Done, a successful action or a change of filter, state or
//! page clears the marks and leaves the mode.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = [
    "[data-select-mode]",
    "[data-select-mode][aria-pressed=\"true\"]",
    ".selecting [data-mark]",
    "[data-mark]:checked",
    ".marked-actions [data-selected-count][aria-live=\"polite\"]",
    "[data-start-marked]",
    "tr.feature.swiped",
    "[data-confirm]",
    ".selecting",
];

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

fn ledger_of(features: Vec<Value>) -> Value {
    json!({"status": 200, "json": {"features": features, "remote_actions": true}})
}

fn ledger() -> Value {
    ledger_of(vec![feature("d1", "draft"), feature("p1", "pending"), feature("p2", "pending"), feature("c1", "completed")])
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run_with(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run_on(true, responses, steps)
}

fn run_on(phone: bool, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/projects/alpha/features", "phone": phone, "viewport": if phone { json!({"width": 390, "height": 844}) } else { json!({"width": 1280, "height": 800}) }, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn run(steps: Vec<Value>) -> Option<Value> {
    run_with(vec![ledger(), projects()], steps)
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

// A finger tap on the row's link: a real click event, which the harness's stubbed link click() would not send.
fn tap(name: &str) -> Value {
    json!({"nav": format!("[data-feature-open=\"{name}\"]")})
}

fn select() -> Value {
    json!({"click": "[data-select-mode]"})
}

#[test]
fn select_shows_the_marks_and_tapping_two_rows_marks_both_without_opening_them() {
    let Some(out) = run(vec![select(), tap("d1"), tap("p1")]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 1, "a Select button: {out}");
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "no marks shown before Select: {out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "the button is pressed: {out}");
    assert!(texts(&frames[2]).iter().any(|t| t == "Done"), "the button now says Done: {out}");
    assert_eq!(count(&frames[2], SELECTORS[2]), 3, "a mark on each draft and pending row: {out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 2, "both rows marked: {out}");
    assert_eq!(frames[4]["settled"]["path"], "/projects/alpha/features", "no feature was opened: {out}");
    assert_eq!(count(&frames[4], SELECTORS[4]), 1, "the bar counts the selection: {out}");
    assert!(texts(&frames[4]).iter().any(|t| t == "2 selected"), "{out}");
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn start_2_from_the_bar_sends_one_group_start_and_a_success_leaves_selection_mode() {
    let profiles = json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}});
    let started = json!({"status": 202, "json": {"run": "run-1"}});
    let steps = vec![select(), tap("p1"), tap("p2"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![ledger(), projects(), profiles, started, ledger(), projects()], steps) else { return };
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Start 2"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "one start request: {out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/p1/runs", "{out}");
    assert_eq!(sent[0]["body"], r#"{"group":["p1","p2"],"implementer":"opus","reviewer":"sonnet"}"#, "{out}");
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "Started group run run-1 for p1, p2."), "the result shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 0, "selection mode is left: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "no marks remain: {out}");
    assert!(texts(end).iter().any(|t| t == "Select"), "the button says Select again: {out}");
}

#[test]
fn done_clears_the_marks_and_hides_the_bar() {
    let Some(out) = run(vec![select(), tap("p1"), tap("d1"), select()]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[1]), 0, "the button is no longer pressed: {out}");
    assert_eq!(count(end, SELECTORS[2]), 0, "the marks are hidden again: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "no marks remain: {out}");
    assert_eq!(count(end, SELECTORS[4]), 0, "the bar is gone: {out}");
    assert_eq!(count(end, SELECTORS[5]), 0, "{out}");
    assert!(posts(&out).is_empty(), "{out}");
}

fn press(name: &str, dy: i64) -> Value {
    json!({"press": format!("[data-feature-open=\"{name}\"]"), "ms": 600, "dy": dy})
}

#[test]
fn a_long_press_enters_selection_mode_with_that_row_marked_and_a_moving_finger_does_not() {
    let Some(out) = run(vec![press("p1", 0)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 1, "selection mode: {out}");
    assert_eq!(count(end, SELECTORS[3]), 1, "the pressed row is marked: {out}");
    assert!(texts(end).iter().any(|t| t == "1 selected"), "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features", "the press opened nothing: {out}");

    let Some(out) = run(vec![press("p1", 30)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "a scroll is no long press: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "{out}");
}

fn swipe(name: &str) -> Value {
    json!({"swipe": format!("[data-feature-row=\"{name}\"]"), "dx": -90})
}

#[test]
fn select_closes_a_swiped_row_and_a_swipe_while_selecting_does_nothing() {
    let Some(out) = run(vec![swipe("p1"), select(), swipe("p2")]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[6]), 1, "the swipe opened the row: {out}");
    assert_eq!(count(&frames[3], SELECTORS[6]), 0, "Select closed it: {out}");
    assert_eq!(count(&frames[4], SELECTORS[6]), 0, "no swipe while selecting: {out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 0, "nor a mark: {out}");
}

#[test]
fn a_list_with_nothing_to_mark_has_no_select_button() {
    let only_done = ledger_of(vec![feature("c1", "completed"), feature("c2", "completed")]);
    let Some(out) = run_with(vec![only_done, projects()], vec![]) else { return };
    assert_eq!(count(last(&out), SELECTORS[0]), 0, "{out}");
}

#[test]
fn a_change_of_state_filter_or_page_leaves_selection_mode() {
    let steps = vec![select(), tap("p1"), json!({"set": "[data-feature-status]", "value": "pending"})];
    let Some(out) = run(steps) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "{out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "the marks are cleared: {out}");

    let steps = vec![select(), tap("p1"), json!({"set": "[data-feature-search]", "value": "p"})];
    let Some(out) = run(steps) else { return };
    assert_eq!(count(last(&out), SELECTORS[8]), 0, "{out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 0, "{out}");
}

#[test]
fn the_css_hides_the_marks_outside_selection_and_keeps_the_bar_inside_the_safe_area() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let phone = &css[css.find("@media (max-width: 700px)").expect("a phone query")..];
    assert!(phone.contains("  .feature-mark, .marked-actions { display: none; }"), "a phone hides the marks unless selecting");
    assert!(css.lines().any(|line| line == ".select-mode { display: none; }"), "a wide screen has no Select button");
    let mark = phone.lines().find(|line| line.starts_with("  .selecting .feature-mark {")).expect("a selecting mark rule");
    assert!(mark.contains("display: inline-block") && mark.contains("min-width: 44px") && mark.contains("min-height: 44px"), "{mark}");
    let bar = phone.lines().find(|line| line.starts_with("  .selecting .marked-actions {")).expect("a selecting bar rule");
    assert!(bar.contains("position: fixed") && bar.contains("env(safe-area-inset-bottom)") && bar.contains("display: flex"), "{bar}");
    assert!(phone.contains("  .feature-list.selecting { padding-bottom:"), "the list keeps room under its last row");
}

#[test]
fn a_long_press_on_a_wide_screen_does_not_enter_selection_mode() {
    let Some(out) = run_on(false, vec![ledger(), projects()], vec![press("p1", 0)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "a wide screen has no selection mode: {out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features/p1", "the press is a plain tap that opens the feature: {out}");
}

// The every-project list puts one Promote and one Start per project in the bar, and Start's question opens
// in it too: the bar wraps them onto more lines and, past a capped height, scrolls rather than pushing a
// button off the screen.
#[test]
fn the_bar_wraps_its_buttons_and_scrolls_past_a_capped_height() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let phone = &css[css.find("@media (max-width: 700px) {\n  .feature-mark").expect("the selection query")..];
    let bar = phone.lines().find(|line| line.starts_with("  .selecting .marked-actions {")).expect("a selecting bar rule");
    assert!(bar.contains("flex-wrap: wrap;") && !bar.contains("nowrap"), "{bar}");
    assert!(bar.contains("max-height:") && bar.contains("overflow-y: auto"), "{bar}");
}

fn gets(out: &Value, url: &str) -> usize {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "GET" && r["url"] == url).count()
}

// A refused start reports the refusal and stays in selection mode with the marks cleared, so the features
// can be marked again and retried; nothing changed, so the ledger is not fetched again.
#[test]
fn a_refused_start_reports_it_and_stays_in_selection_mode_with_the_marks_cleared() {
    let profiles = json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}});
    let refused = json!({"status": 409, "json": {"error": "feature p1 already has a run"}});
    let steps = vec![select(), tap("p1"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![ledger(), projects(), profiles, refused], steps) else { return };
    let end = last(&out);
    assert_eq!(posts(&out).len(), 1, "{out}");
    assert!(texts(end).iter().any(|t| t.contains("feature p1 already has a run")), "the refusal shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 1, "still selecting: {out}");
    assert_eq!(count(end, SELECTORS[1]), 1, "the button still says Done: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "the marks are cleared: {out}");
    assert_eq!(count(end, SELECTORS[4]), 0, "with nothing marked the bar is gone: {out}");
    assert_eq!(gets(&out, "/api/projects/alpha/features"), 1, "no refresh: {out}");
}

// A promote of two where one is refused still changed the ledger: it reports both, refreshes the list and
// leaves selection mode.
#[test]
fn a_promote_with_one_refusal_reports_both_refreshes_and_leaves_selection_mode() {
    let drafts = || ledger_of(vec![feature("d1", "draft"), feature("d2", "draft"), feature("p1", "pending")]);
    let ok = json!({"status": 200, "json": {"feature": "d1", "status": "pending"}});
    let refused = json!({"status": 409, "json": {"error": "feature d2: Pending to Pending is not allowed"}});
    let steps = vec![select(), tap("d1"), tap("d2"), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![drafts(), projects(), ok, refused, drafts(), projects()], steps) else { return };
    let end = last(&out);
    let sent = posts(&out);
    let urls: Vec<&str> = sent.iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["/api/projects/alpha/features/d1/promote", "/api/projects/alpha/features/d2/promote"], "{out}");
    assert!(texts(end).iter().any(|t| t.contains("feature d2: Pending to Pending is not allowed")), "the refusal shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 0, "selection mode is left: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "{out}");
    assert_eq!(gets(&out, "/api/projects/alpha/features"), 2, "the list is refreshed: {out}");
}
