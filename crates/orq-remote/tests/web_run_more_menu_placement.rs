//! On a wide screen a run row's ⋯ menu, and any question one of its actions asks (Discard, Clear, Stop, Accept,
//! Resume, Switch model), is placed against the window rather than inside the run table, whose sideways scroll
//! would otherwise cut it off: below the ⋯ button with its right edge under the button's, above the button
//! when there is no room below, and never past the window's right edge. A phone keeps showing the actions in
//! the row, so nothing is placed there.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const MENU: &str = ".run-more-menu";
const TOGGLE: &str = "[data-run-more]";

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

/// A rect for the harness: the box an element matching `selector` has on screen.
fn rect(selector: &str, top: i64, left: i64, width: i64, height: i64) -> Value {
    json!({"selector": selector, "top": top, "left": left, "width": width, "height": height})
}

/// One failed run's row in a 1280×800 window, with the ⋯ button and the menu where `rects` put them.
fn failed_row(rects: Vec<Value>, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let body = json!({"runs": [one("r-fail", "failed")], "next_cursor": null, "total": 1, "running": 0, "remote_actions": true});
    let mut responses = vec![json!({"status": 200, "json": body}), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": responses, "steps": all, "viewport": {"width": 1280, "height": 800}, "rects": rects, "styles": [MENU], "selectors": [".run-more-menu .confirm"]}))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn style(frame: &Value) -> String {
    frame["settled"]["styles"][MENU].as_str().unwrap_or("").to_string()
}

#[test]
fn an_open_menu_with_room_below_sits_under_the_dots_button_right_edges_together() {
    let rects = vec![rect(TOGGLE, 200, 960, 36, 36), rect(MENU, 0, 0, 256, 120)];
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE})]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("top: 240px"), "4px under the button's bottom at 236: {placed:?} {out}");
    assert!(placed.contains("left: 740px"), "right edge under the button's at 996: {placed:?} {out}");
}

#[test]
fn an_open_menu_without_room_below_sits_above_the_dots_button() {
    let rects = vec![rect(TOGGLE, 700, 960, 36, 36), rect(MENU, 0, 0, 256, 200)];
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE})]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("top: 496px"), "its bottom 4px over the button's top at 700: {placed:?} {out}");
    assert!(placed.contains("left: 740px"), "{placed:?} {out}");
}

#[test]
fn the_discard_question_growing_the_menu_past_the_bottom_moves_it_above_the_button() {
    let rects = vec![rect(TOGGLE, 560, 960, 36, 36), rect(".run-more-menu:has(.confirm)", 0, 0, 256, 200), rect(MENU, 0, 0, 256, 120)];
    let warning = json!({"status": 200, "json": {"run": "r-fail", "warning": ""}});
    let Some(out) = failed_row(rects, vec![warning], vec![json!({"click": TOGGLE}), json!({"click": "[data-discard]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(style(&frames[2]).contains("top: 600px"), "the closed-up menu fits below: {out}");
    let end = last(&out);
    assert_eq!(end["settled"]["matches"][".run-more-menu .confirm"], 1, "{out}");
    let placed = style(end);
    assert!(placed.contains("top: 356px"), "the question no longer fits below, so it goes above: {placed:?} {out}");
}

#[test]
fn an_open_menu_whose_button_runs_past_the_right_edge_stays_inside_the_window() {
    let rects = vec![rect(TOGGLE, 200, 1250, 36, 36), rect(MENU, 0, 0, 256, 120)];
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE})]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("left: 1016px"), "8px in from the 1280px window's right edge: {placed:?} {out}");
}

#[test]
fn an_open_menu_wider_than_the_window_starts_at_its_left_edge() {
    let rects = vec![rect(TOGGLE, 200, 100, 36, 36), rect(MENU, 0, 0, 1400, 120)];
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE})]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("left: 8px"), "{placed:?} {out}");
}

#[test]
fn an_open_menu_follows_its_button_when_the_run_table_scrolls() {
    let rects = vec![rect(TOGGLE, 200, 960, 36, 36), rect(MENU, 0, 0, 256, 120)];
    let moved = json!({"rects": [rect(TOGGLE, 200, 900, 36, 36), rect(MENU, 0, 0, 256, 120)], "event": "scroll", "on": ".feature-table-wrap"});
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE}), moved]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("left: 680px"), "{placed:?} {out}");
}

#[test]
fn an_open_menu_follows_its_button_when_the_window_changes_size() {
    let rects = vec![rect(TOGGLE, 200, 960, 36, 36), rect(MENU, 0, 0, 256, 120)];
    let moved = json!({"rects": [rect(TOGGLE, 300, 960, 36, 36), rect(MENU, 0, 0, 256, 120)], "event": "resize"});
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE}), moved]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("top: 340px"), "{placed:?} {out}");
}

#[test]
fn an_open_menu_too_tall_for_either_side_of_its_button_stays_inside_the_window() {
    let rects = vec![rect(TOGGLE, 400, 960, 36, 36), rect(MENU, 0, 0, 256, 700)];
    let Some(out) = failed_row(rects, vec![], vec![json!({"click": TOGGLE})]) else { return };
    let placed = style(last(&out));
    assert!(placed.contains("top: 92px"), "its bottom 8px above the 800px window's: {placed:?} {out}");
}

#[test]
fn the_menu_is_placed_against_the_window_and_scrolls_when_taller_than_it() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let rule = css.lines().find(|line| line.starts_with(".run-more-menu {")).expect("the menu's rule");
    assert!(rule.contains("position: fixed;"), "out of the run table's sideways scroll: {rule}");
    assert!(!rule.contains("top:") && !rule.contains("right:"), "the page places it: {rule}");
    assert!(rule.contains("max-height: calc(100vh - 1rem);") && rule.contains("overflow-y: auto;"), "{rule}");
}
