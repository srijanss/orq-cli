//! The page's buttons share one style: a primary button (Start, Promote, Sign in…) is filled with the accent
//! blue and white text in both themes, a secondary button (Queue, Start with…, Edit) beside it is the surface
//! colour with a border and is never also a primary button, and every button lays its icon and label out in
//! one inline-flex row with a gap between them. A feature's actions sit in one row with any note on why a
//! start is held back once under it, and the Features list's selection bar puts Start N and Queue N in one
//! row. Rendered in jsdom for the markup and measured in a real headless Chromium for the layout and colours,
//! at desktop and phone widths, in both themes.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const REASON: &str = "alpha already has 1 active run (run-seed, COM-02): wait for it to finish, or raise the limit in project settings";

fn project(blocked: Value) -> Value {
    json!({"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 0, "max_active_runs": null, "start_blocked": blocked, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn ledger(blocked: Value) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"features": [feature("p1", "pending"), feature("p2", "pending"), feature("sketch", "draft")], "remote_actions": true, "start_blocked": blocked})
}

fn no_runs() -> Value {
    json!({"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true})
}

fn jsdom(url: &str, blocked: Value, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let responses = vec![json!({"status": 200, "json": ledger(blocked.clone())}), json!({"status": 200, "json": no_runs()}), json!({"status": 200, "json": {"projects": [project(blocked)]}})];
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(out: &Value, selector: &str) -> u64 {
    out["frames"].as_array().unwrap().last().unwrap()["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("no count for {selector}: {out}"))
}

const DETAIL: [&str; 7] = [
    ".feature-detail-actions button.primary-button[data-start]",
    ".feature-detail-actions button.secondary-button[data-start-with]:not(.primary-button)",
    ".feature-detail-actions button.secondary-button[data-queue-feature]:not(.primary-button)",
    ".feature-detail-actions button.secondary-button[data-feature-edit]:not(.primary-button)",
    "[data-start-blocked]",
    ".feature-detail-actions [data-start-blocked]",
    ".feature-detail-actions + [data-start-blocked]",
];

#[test]
fn a_features_start_is_primary_its_other_actions_secondary_and_a_held_back_start_says_why_once_under_the_row() {
    let Some(out) = jsdom("/projects/alpha/features/p1", Value::Null, &DETAIL) else { return };
    for selector in &DETAIL[..4] {
        assert_eq!(count(&out, selector), 1, "{selector}: {out}");
    }
    assert_eq!(count(&out, DETAIL[4]), 0, "free to start, no note: {out}");
    let Some(out) = jsdom("/projects/alpha/features/p1", json!(REASON), &DETAIL) else { return };
    assert_eq!(count(&out, DETAIL[4]), 1, "the note shows once: {out}");
    assert_eq!(count(&out, DETAIL[5]), 0, "not inside the row: {out}");
    assert_eq!(count(&out, DETAIL[6]), 1, "under the row: {out}");
}

#[test]
fn the_selection_bars_queue_and_promote_are_secondary_and_its_note_shows_once() {
    let selectors = ["[data-queue-marked].secondary-button:not(.primary-button)", "[data-promote-marked].secondary-button:not(.primary-button)", "[data-start-marked].primary-button", "[data-start-blocked]", ".marked-group > [data-start-blocked]"];
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let responses = vec![json!({"status": 200, "json": ledger(json!(REASON))}), json!({"status": 200, "json": {"projects": [project(json!(REASON))]}})];
    let steps = vec![json!({"submit": TOKEN}), json!({"click": "[data-mark=\"p1\"]"}), json!({"click": "[data-mark=\"sketch\"]"})];
    let scenario = json!({"url": "/projects/alpha/features", "responses": responses, "steps": steps, "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    for selector in selectors {
        assert_eq!(count(&out, selector), 1, "{selector}: {out}");
    }
}

#[test]
fn the_tokens_make_primary_the_accent_blue_in_every_theme_and_secondary_is_global() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    assert_eq!(css.matches("--primary: #2563eb;").count(), 4, "every theme block");
    assert_eq!(css.matches("--on-primary: #ffffff;").count(), 4, "every theme block");
    assert_eq!(css.matches("--primary-strong: #1d4ed8;").count(), 4, "hover and active, every theme block");
    assert!(!css.contains(".signin .primary {"), "the sign-in button is a primary button");
    assert!(css.lines().any(|line| line.starts_with(".secondary-button {")), "a global secondary button");
    assert!(css.contains(":focus-visible { outline: 3px solid var(--accent); outline-offset: 2px; }"), "the focus ring stays");
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap();
    assert!(!main.contains("\"primary-button secondary-button\""), "a secondary button is not also primary");
    assert!(!main.contains("className=\"primary\""), "the sign-in buttons are primary buttons");
}

struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

fn rects(result: &Value, selector: &str) -> Vec<Rect> {
    result["boxes"][selector]
        .as_array()
        .unwrap_or_else(|| panic!("no boxes for {selector}: {result}"))
        .iter()
        .map(|b| Rect { left: b["left"].as_f64().unwrap(), top: b["top"].as_f64().unwrap(), right: b["right"].as_f64().unwrap(), bottom: b["bottom"].as_f64().unwrap() })
        .filter(|r| r.right > r.left)
        .collect()
}

fn style<'a>(result: &'a Value, selector: &str, prop: &str) -> &'a str {
    result["styles"][selector][0][prop].as_str().unwrap_or_else(|| panic!("no {prop} for {selector}: {result}"))
}

fn replies(blocked: Value) -> Vec<web_chromium::Reply> {
    vec![
        web_chromium::get("/api/projects", json!({"projects": [project(blocked.clone())]})),
        web_chromium::get("/api/projects/alpha/features", ledger(blocked)),
        web_chromium::get("/api/projects/alpha/runs*", no_runs()),
    ]
}

fn load(chromium: &std::path::Path, theme: &str, width: u32, url: &str, blocked: Value, mut steps: Vec<Value>) -> Value {
    let init = format!("try {{ localStorage.setItem(\"orq.theme\", \"{theme}\"); }} catch (e) {{}}");
    steps.push(json!({"tokens": ["--surface", "--border", "--text"]}));
    let measured = web_chromium::measure_with(chromium, width, url, &replies(blocked), Value::Array(steps), &init);
    assert!(measured.result.get("error").is_none(), "{theme} {width}: {} (unanswered {:?})", measured.result, measured.unanswered);
    let result = measured.result;
    assert!(result["scrollWidth"].as_u64().unwrap() <= u64::from(width), "{theme} {width}: no horizontal scroll: {result}");
    assert_eq!(result["offenders"].as_array().map(Vec::len), Some(0), "{theme} {width}: nothing past the right edge: {result}");
    result
}

const START: &str = ".feature-detail-actions [data-start]";
const ROW: [&str; 4] = [".feature-detail-actions [data-start]", ".feature-detail-actions [data-start-with]", ".feature-detail-actions [data-queue-feature]", ".feature-detail-actions [data-feature-edit]"];
const COLOURS: [&str; 4] = ["background-color", "color", "border-top-width", "border-top-color"];

#[test]
fn start_is_blue_with_white_text_and_queue_the_surface_with_a_border_in_both_themes() {
    let Some(chromium) = web_chromium::binary() else { return };
    for theme in ["light", "dark"] {
        let steps = vec![json!({"wait": START}), json!({"styles": START, "props": COLOURS}), json!({"styles": ROW[2], "props": COLOURS})];
        let result = load(&chromium, theme, 1280, "/projects/alpha/features/p1", Value::Null, steps);
        assert_eq!(style(&result, START, "background-color"), "rgb(37, 99, 235)", "{theme}: {result}");
        assert_eq!(style(&result, START, "color"), "rgb(255, 255, 255)", "{theme}: {result}");
        assert_eq!(style(&result, ROW[2], "background-color"), result["tokens"]["--surface"].as_str().unwrap(), "{theme}: {result}");
        assert_eq!(style(&result, ROW[2], "color"), result["tokens"]["--text"].as_str().unwrap(), "{theme}: {result}");
        assert_eq!(style(&result, ROW[2], "border-top-width"), "1px", "{theme}: {result}");
        assert_eq!(style(&result, ROW[2], "border-top-color"), result["tokens"]["--border"].as_str().unwrap(), "{theme}: {result}");
    }
}

#[test]
fn a_plain_button_with_an_icon_keeps_a_gap_between_icon_and_label() {
    let Some(chromium) = web_chromium::binary() else { return };
    let plain = "button[data-improve-draft]";
    for theme in ["light", "dark"] {
        let steps = vec![json!({"wait": plain}), json!({"styles": plain, "props": ["display", "column-gap", "font-weight", "min-height", "border-top-left-radius"]}), json!({"boxes": format!("{plain} > svg")})];
        let result = load(&chromium, theme, 1280, "/projects/alpha/features/sketch", Value::Null, steps);
        // A flex row's items are blockified, so inside the actions row an inline-flex button computes as flex.
        assert!(["inline-flex", "flex"].contains(&style(&result, plain, "display")), "{theme}: {result}");
        let gap: f64 = style(&result, plain, "column-gap").trim_end_matches("px").parse().unwrap_or(0.0);
        assert!(gap >= 6.0, "{theme}: a gap of at least 6px between icon and label: {result}");
        assert_eq!(style(&result, plain, "font-weight"), "600", "{theme}: {result}");
        assert_eq!(style(&result, plain, "min-height"), "44px", "{theme}: {result}");
        assert_eq!(style(&result, plain, "border-top-left-radius"), "8px", "{theme}: {result}");
        let icon = &rects(&result, &format!("{plain} > svg"))[0];
        assert_eq!((icon.right - icon.left, icon.bottom - icon.top), (16.0, 16.0), "{theme}: a 16px icon: {result}");
    }
}

fn assert_one_row(result: &Value, selectors: &[&str], at: &str) {
    let tops: Vec<f64> = selectors.iter().map(|s| rects(result, s).first().unwrap_or_else(|| panic!("{at}: {s} shown: {result}")).top).collect();
    assert!(tops.iter().all(|top| (top - tops[0]).abs() <= 1.0), "{at}: {selectors:?} share one row, tops {tops:?}: {result}");
}

fn assert_tall_and_inside(result: &Value, selector: &str, width: u32, at: &str) {
    let all = rects(result, selector);
    assert!(!all.is_empty(), "{at}: {selector} shown: {result}");
    for each in all {
        assert!(each.bottom - each.top >= 44.0, "{at}: {selector} at least 44px tall: {result}");
        assert!(each.left >= 0.0 && each.right <= f64::from(width), "{at}: {selector} inside the page: {result}");
    }
}

#[test]
fn a_features_actions_share_one_row_with_the_held_back_note_once_under_it() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let mut steps = vec![json!({"wait": START})];
        for selector in ROW.iter().chain([".feature-detail-actions button", "[data-start-blocked]"].iter()) {
            steps.push(json!({"boxes": selector}));
        }
        let at = format!("{width} px");
        let result = load(&chromium, "light", width, "/projects/alpha/features/p1", json!(REASON), steps);
        if width == 1280 {
            assert_one_row(&result, &ROW, &at);
        } else {
            assert_one_row(&result, &ROW[..2], &at);
        }
        assert_tall_and_inside(&result, ".feature-detail-actions button", width, &at);
        let notes = rects(&result, "[data-start-blocked]");
        assert_eq!(notes.len(), 1, "{at}: the note once: {result}");
        let lowest = ROW.iter().map(|s| rects(&result, s)[0].bottom).fold(0.0, f64::max);
        assert!(notes[0].top >= lowest, "{at}: the note under the row: {result}");
    }
}

#[test]
fn the_selection_bars_start_and_queue_share_one_row_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    let pair = ["[data-start-marked]", "[data-queue-marked]"];
    for width in [1280, 390] {
        let mut steps = if width == 390 { vec![json!({"click": "[data-select-mode]"})] } else { vec![] };
        steps.push(json!({"click": "[data-mark=\"p1\"]"}));
        steps.push(json!({"click": "[data-mark=\"p2\"]"}));
        steps.push(json!({"wait": "[data-queue-marked]"}));
        for selector in pair.iter().chain([".marked-actions button", "[data-start-blocked]"].iter()) {
            steps.push(json!({"boxes": selector}));
        }
        let at = format!("{width} px");
        let result = load(&chromium, "dark", width, "/projects/alpha/features", json!(REASON), steps);
        assert_one_row(&result, &pair, &at);
        assert_tall_and_inside(&result, ".marked-actions button", width, &at);
        let notes = rects(&result, "[data-start-blocked]");
        assert_eq!(notes.len(), 1, "{at}: the note once: {result}");
        let lowest = pair.iter().map(|s| rects(&result, s)[0].bottom).fold(0.0, f64::max);
        assert!(notes[0].top >= lowest, "{at}: the note under the buttons: {result}");
        if width == 390 {
            let (start, queue) = (&rects(&result, pair[0])[0], &rects(&result, pair[1])[0]);
            assert!(((start.right - start.left) - (queue.right - queue.left)).abs() <= 2.0, "{at}: each half the width: {result}");
        }
    }
}
