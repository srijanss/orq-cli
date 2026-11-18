//! On a wide screen, the question a run's row action asks (such as "Resume r-fail from implement?") floats in
//! a panel anchored to its button instead of opening inside the table cell, so the row and its columns keep
//! their size. Cancel, Escape or a press elsewhere closes it; focus moves into the panel and back to the
//! action's button. Only one such panel or ⋯ menu is open at a time. The run page and a phone still ask in place.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const PANEL: &str = "[data-run-action-float]";
const SELECTORS: &[&str] = &[
    "[data-run-action-float]",
    "td [data-run-action-float]",
    "td .confirm",
    "[data-run-action-float] .confirm [data-confirm]",
    "[data-run-action-float] .confirm [data-cancel]",
    "td [data-resume]",
    "td [data-resume][disabled]",
    ".run-action-note.error-text",
    ".run-more.open",
    ".run-action > .confirm",
    ".confirm",
];

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

fn checked(run: &str) -> Value {
    json!({"status": 200, "json": {"run": run, "next": "implement"}})
}

/// The Orchestrations list with a failed and a paused run, both offering Resume and more behind ⋯.
fn runs_page(extra: Vec<Value>, steps: Vec<Value>, phone: bool) -> Option<Value> {
    let rows = [one("r-fail", "failed"), one("r-paused", "paused")];
    let body = json!({"runs": rows, "next_cursor": null, "total": 2, "running": 0, "remote_actions": true});
    let mut responses = vec![json!({"status": 200, "json": body}), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": all, "selectors": SELECTORS, "phone": phone}))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} is not measured"))
}

fn focused(frame: &Value, selector: &str) -> bool {
    frame["settled"]["focusMatches"][selector].as_bool().unwrap_or_else(|| panic!("{selector} is not measured"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn resume(index: usize) -> Value {
    json!({"click": "td [data-resume]", "index": index})
}

#[test]
fn a_rows_question_floats_outside_its_cell_with_both_answers_and_takes_focus() {
    let Some(out) = runs_page(vec![checked("r-fail")], vec![resume(0)], false) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, PANEL), count(frame, "td [data-run-action-float]"), count(frame, "td .confirm")), (1, 0, 0), "{out}");
    assert_eq!(count(frame, "[data-run-action-float] .confirm [data-confirm]"), 1, "{out}");
    assert_eq!(count(frame, "[data-run-action-float] .confirm [data-cancel]"), 1, "{out}");
    assert!(texts(frame).iter().any(|t| t.contains("Resume r-fail from implement?")), "{:?}", texts(frame));
    assert_eq!(count(frame, "td [data-resume]"), 2, "the button stays in its row, so the row keeps its size: {out}");
    assert!(focused(frame, "[data-run-action-float] .confirm [data-confirm]"), "focus moves to the panel's first button: {out}");
}

#[test]
fn cancel_closes_the_panel_and_returns_focus_to_the_action_button() {
    let steps = vec![resume(0), json!({"click": "[data-run-action-float] .confirm [data-cancel]"})];
    let Some(out) = runs_page(vec![checked("r-fail")], steps, false) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, PANEL), 0, "{out}");
    assert!(focused(frame, "td [data-resume]"), "focus is back on Resume: {out}");
}

#[test]
fn escape_closes_the_panel_and_returns_focus_to_the_action_button() {
    let steps = vec![resume(0), json!({"keydown": "Escape"})];
    let Some(out) = runs_page(vec![checked("r-fail")], steps, false) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, PANEL), 0, "{out}");
    assert!(focused(frame, "td [data-resume]"), "focus is back on Resume: {out}");
}

#[test]
fn a_press_outside_the_panel_closes_it() {
    // Nothing moves; a press reaches the page's heading, outside the panel and its button.
    let steps = vec![resume(0), json!({"rects": [], "event": "pointerdown", "on": "h1"})];
    let Some(out) = runs_page(vec![checked("r-fail")], steps, false) else { return };
    assert_eq!(count(last(&out), PANEL), 0, "{out}");
}

#[test]
fn opening_a_panel_closes_an_open_dots_menu_and_opening_a_menu_closes_the_panel() {
    let steps = vec![json!({"click": "[data-run-more]", "index": 0}), resume(1)];
    let Some(out) = runs_page(vec![checked("r-paused")], steps, false) else { return };
    assert_eq!(count(&out["frames"][2], ".run-more.open"), 1, "{out}");
    assert_eq!((count(last(&out), ".run-more.open"), count(last(&out), PANEL)), (0, 1), "{out}");
    let steps = vec![resume(0), json!({"click": "[data-run-more]", "index": 1})];
    let Some(out) = runs_page(vec![checked("r-fail")], steps, false) else { return };
    assert_eq!((count(last(&out), ".run-more.open"), count(last(&out), PANEL)), (1, 0), "{out}");
}

#[test]
fn only_one_panel_is_open_at_a_time() {
    let steps = vec![resume(0), resume(1)];
    let Some(out) = runs_page(vec![checked("r-fail"), checked("r-paused")], steps, false) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, PANEL), 1, "{out}");
    assert!(texts(frame).iter().any(|t| t.contains("Resume r-paused from implement?")), "{:?}", texts(frame));
    assert!(!texts(frame).iter().any(|t| t.contains("Resume r-fail from implement?")), "{:?}", texts(frame));
}

fn posts(out: &Value) -> usize {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").count()
}

#[test]
fn opening_a_menu_while_a_confirmed_action_is_sending_cannot_send_it_again() {
    // The resume is confirmed and its POST has no answer yet; a ⋯ menu opens, then Resume is pressed again.
    let held = json!({"status": 202, "json": {"run": "r-next"}, "hold": "fetch"});
    let steps = vec![resume(0), json!({"click": "[data-run-action-float] .confirm [data-confirm]"}), json!({"click": "[data-run-more]", "index": 1}), resume(0)];
    let Some(out) = runs_page(vec![checked("r-fail"), held, checked("r-fail")], steps, false) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, ".run-more.open"), 1, "{out}");
    assert_eq!(count(frame, "td [data-resume][disabled]"), 1, "Resume waits for its answer: {out}");
    assert_eq!(posts(&out), 1, "{out}");
    let checks = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/projects/alpha/runs/r-fail/resume-check").count();
    assert_eq!(checks, 1, "Resume is not asked again while it is sending: {out}");
}

#[test]
fn on_a_phone_the_question_still_opens_in_its_row() {
    let Some(out) = runs_page(vec![checked("r-fail")], vec![resume(0)], true) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, PANEL), count(frame, "td .confirm")), (0, 1), "{out}");
}

#[test]
fn the_run_page_still_asks_in_place() {
    let body = json!({"run": one("r-fail", "failed"), "events": [], "next_cursor": null, "remote_actions": true});
    let responses = json!([{"status": 200, "json": body}, projects(), {"status": 200, "stream": [], "end": "hold"}, checked("r-fail")]);
    let steps = json!([{"submit": TOKEN}, {"click": "[data-resume]"}]);
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations/r-fail", "responses": responses, "steps": steps, "selectors": SELECTORS})) else { return };
    let frame = last(&out);
    assert_eq!((count(frame, PANEL), count(frame, ".run-action > .confirm")), (0, 1), "{out}");
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

fn rects(result: &Value, selector: &str) -> Vec<Rect> {
    result["boxes"][selector]
        .as_array()
        .unwrap_or_else(|| panic!("no boxes for {selector}: {result}"))
        .iter()
        .map(|b| Rect { left: b["left"].as_i64().unwrap(), top: b["top"].as_i64().unwrap(), right: b["right"].as_i64().unwrap(), bottom: b["bottom"].as_i64().unwrap() })
        .collect()
}

#[test]
fn on_a_desktop_the_panel_is_wide_enough_inside_the_window_and_the_row_keeps_its_height() {
    let Some(chromium) = web_chromium::binary() else { return };
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let rows = [one("r-fail", "failed"), one("r-paused", "paused")];
    let replies = [
        web_chromium::get("/api/projects", json!({"projects": [project]})),
        web_chromium::get("/api/projects/alpha/runs/r-fail/resume-check", json!({"run": "r-fail", "next": "implement"})),
        web_chromium::get("/api/projects/alpha/runs*", json!({"runs": rows, "next_cursor": null, "total": 2, "running": 0, "remote_actions": true})),
    ];
    let url = "/projects/alpha/orchestrations";
    let before = web_chromium::measure(&chromium, 1280, url, &replies, json!([{"wait": "td [data-resume]"}, {"boxes": "tr.run-row"}])).result;
    assert!(before.get("error").is_none(), "{before}");
    let steps = json!([{"wait": "td [data-resume]"}, {"click": "td [data-resume]"}, {"wait": "[data-run-action-float] .confirm"}, {"boxes": "tr.run-row"}, {"boxes": PANEL}, {"boxes": "td [data-resume]"}]);
    let after = web_chromium::measure(&chromium, 1280, url, &replies, steps).result;
    assert!(after.get("error").is_none(), "{after}");
    assert!(after["scrollWidth"].as_u64() <= after["clientWidth"].as_u64(), "the page scrolls sideways: {after}");
    let panel = rects(&after, PANEL)[0];
    assert!(panel.right - panel.left >= 256, "the panel {panel:?} is at least 16rem wide");
    assert!(panel.left >= 0 && panel.right <= 1280 && panel.top >= 0 && panel.bottom <= 800, "the panel {panel:?} is inside the window");
    let button = rects(&after, "td [data-resume]")[0];
    assert!(panel.top >= button.bottom || panel.bottom <= button.top, "the panel {panel:?} sits under or over its button {button:?}");
    let (was, is) = (rects(&before, "tr.run-row"), rects(&after, "tr.run-row"));
    for at in 0..2 {
        assert_eq!(is[at].bottom - is[at].top, was[at].bottom - was[at].top, "row {at} keeps its height: {:?} then {:?}", was[at], is[at]);
        assert_eq!(is[at].right - is[at].left, was[at].right - was[at].left, "row {at} keeps its width");
    }
}
