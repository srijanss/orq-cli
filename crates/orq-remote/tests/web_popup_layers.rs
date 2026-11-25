//! The page's popups share one layer scale, one scrim and one shadow per depth: a modal's backdrop dims the
//! page in both themes, the update dialog sits above an open ⋯ menu, every overlay's z-index is its scale
//! token, and the two project pickers' lists are at least as wide as their button, grow to their rows up to
//! 60vh, scroll inside and show the chosen row when opened. Measured in a real headless Chromium at 1280 and
//! 390 wide, light and dark.

mod web_chromium;

use serde_json::{json, Value};
use std::path::Path;

fn project(id: &str) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn projects(count: usize) -> Value {
    json!({"projects": (1..=count).map(|n| project(&format!("p{n:02}"))).collect::<Vec<_>>()})
}

fn ledger() -> Value {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"features": [feature("f1"), feature("f2")], "remote_actions": true, "start_blocked": null})
}

fn failed_runs() -> Value {
    let run = json!({"run_id": "r-fail", "feature": "f-r-fail", "state": "failed", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    json!({"runs": [run], "next_cursor": null, "total": 1, "running": 0, "remote_actions": true})
}

fn replies(count: usize) -> Vec<web_chromium::Reply> {
    let mut all = vec![web_chromium::get("/api/projects", projects(count)), web_chromium::get("/api/todo", json!({"todos": [], "remote_actions": true}))];
    for n in 1..=count {
        let id = format!("p{n:02}");
        all.push(web_chromium::get(&format!("/api/projects/{id}/features"), ledger()));
        all.push(web_chromium::get(&format!("/api/projects/{id}/runs/r-fail/resume-check"), json!({"run": "r-fail", "next": "implement"})));
        all.push(web_chromium::get(&format!("/api/projects/{id}/runs*"), failed_runs()));
    }
    all
}

/// A service worker that has a new release waiting, so the update dialog opens over the page.
const WAITING_UPDATE: &str = "(function () { var waiting = {state: \"installed\", postMessage: function () {}}; var registration = {waiting: waiting, installing: null, addEventListener: function () {}}; var container = {controller: {}, register: function () { return Promise.resolve(registration); }, addEventListener: function () {}}; Object.defineProperty(Navigator.prototype, \"serviceWorker\", {configurable: true, get: function () { return container; }}); })();";

fn load(chromium: &Path, theme: &str, width: u32, url: &str, count: usize, update: bool, steps: Vec<Value>) -> Value {
    let mut init = format!("try {{ localStorage.setItem(\"orq.theme\", \"{theme}\"); }} catch (e) {{}}");
    if update {
        init.push_str(WAITING_UPDATE);
    }
    let measured = web_chromium::measure_with(chromium, width, url, &replies(count), Value::Array(steps), &init);
    assert!(measured.result.get("error").is_none(), "{theme} {width} {url}: {} (unanswered {:?})", measured.result, measured.unanswered);
    measured.result
}

/// An sRGB colour with alpha from a computed value: `rgb(r, g, b)`, `rgba(r, g, b, a)` or `color(srgb r g b / a)`.
fn colour(value: &str) -> [f64; 4] {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix("color(srgb ").and_then(|v| v.strip_suffix(')')) {
        let (rgb, alpha) = inner.split_once('/').map_or((inner, 1.0), |(c, a)| (c, a.trim().parse().unwrap()));
        let parts: Vec<f64> = rgb.split_whitespace().map(|p| p.parse::<f64>().unwrap() * 255.0).collect();
        return [parts[0], parts[1], parts[2], alpha];
    }
    let inner = value.split_once('(').and_then(|(_, rest)| rest.strip_suffix(')')).unwrap_or_else(|| panic!("not a colour: {value}"));
    let parts: Vec<f64> = inner.split(|c| c == ',' || c == '/' || c == ' ').filter(|p| !p.is_empty()).map(|p| p.parse().unwrap()).collect();
    [parts[0], parts[1], parts[2], parts.get(3).copied().unwrap_or(1.0)]
}

fn luminance([r, g, b, _]: [f64; 4]) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn style<'a>(result: &'a Value, selector: &str, prop: &str) -> &'a str {
    result["styles"][selector][0][prop].as_str().unwrap_or_else(|| panic!("no {prop} for {selector}: {result}"))
}

#[test]
fn a_modal_backdrop_dims_the_page_in_both_themes() {
    let Some(chromium) = web_chromium::binary() else { return };
    for theme in ["dark", "light"] {
        for width in [1280, 390] {
            let steps = vec![json!({"click": "[data-dashboard-add]"}), json!({"wait": ".feature-modal"}), json!({"styles": ".feature-modal", "props": ["background-color"]}), json!({"tokens": ["--bg", "--scrim"]})];
            let result = load(&chromium, theme, width, "/dashboard", 2, false, steps);
            let page = colour(result["tokens"]["--bg"].as_str().unwrap());
            let scrim = colour(style(&result, ".feature-modal", "background-color"));
            assert!(scrim[3] > 0.0, "{theme} {width}: the backdrop has a colour: {result}");
            let mixed = [0, 1, 2].map(|i| scrim[i] * scrim[3] + page[i] * (1.0 - scrim[3]));
            let behind = luminance([mixed[0], mixed[1], mixed[2], 1.0]);
            assert!(behind < luminance(page) - 1.0, "{theme} {width}: the page behind the backdrop ({mixed:?}) is darker than without it ({page:?}): {result}");
            assert_eq!(style(&result, ".feature-modal", "background-color"), result["tokens"]["--scrim"].as_str().unwrap(), "{theme} {width}: the backdrop is the scrim token");
        }
    }
}

const ABOVE_MENU: &str = "var card = document.querySelector('.update-card').getBoundingClientRect();
var onCard = document.elementFromPoint(card.left + card.width / 2, card.top + card.height / 2);
var menu = document.querySelector('.run-more.open .run-more-menu').getBoundingClientRect();
var onMenu = document.elementFromPoint(menu.left + menu.width / 2, menu.top + menu.height / 2);
return {card: onCard !== null && onCard.closest('.update-card') !== null, menuUnderScrim: onMenu !== null && onMenu.classList.contains('update-scrim'), menuWidth: menu.width};";

#[test]
fn the_update_dialog_is_above_an_open_more_menu() {
    let Some(chromium) = web_chromium::binary() else { return };
    for theme in ["light", "dark"] {
        let steps = vec![json!({"wait": ".update-card"}), json!({"click": "[data-run-more]"}), json!({"wait": ".run-more.open"}), json!({"eval": "top", "js": ABOVE_MENU})];
        let result = load(&chromium, theme, 1280, "/projects/p01/orchestrations", 1, true, steps);
        let top = &result["evals"]["top"];
        assert!(top["menuWidth"].as_f64().unwrap_or(0.0) > 0.0, "{theme}: the menu is open: {result}");
        assert_eq!(top["card"], true, "{theme}: the dialog is on top at its centre: {result}");
        assert_eq!(top["menuUnderScrim"], true, "{theme}: the scrim covers the open menu: {result}");
    }
}

const LAYERS: &str = "var root = getComputedStyle(document.documentElement); var out = {};
['--z-chrome', '--z-sheet', '--z-menu', '--z-scrim', '--z-dialog', '--z-fab'].forEach(function (t) { out[t] = root.getPropertyValue(t).trim(); });
return out;";

fn assert_layers(result: &Value, expected: &[(&str, &str)], at: &str) {
    let tokens = &result["evals"]["layers"];
    for (token, number) in [("--z-chrome", "10"), ("--z-sheet", "20"), ("--z-menu", "30"), ("--z-scrim", "40"), ("--z-dialog", "50"), ("--z-fab", "60")] {
        assert_eq!(tokens[token], number, "{at}: {token} is {number}: {result}");
    }
    for (selector, token) in expected {
        let all = result["styles"][selector].as_array().unwrap_or_else(|| panic!("{result}\n{at}: no styles for {selector}"));
        assert!(!all.is_empty(), "{result}\n{at}: {selector} on the page");
        assert_eq!(all[0]["z-index"], tokens[token], "{result}\n{at}: {selector} sits on {token}");
    }
}

fn layer_steps(mut steps: Vec<Value>, selectors: &[(&str, &str)]) -> Vec<Value> {
    for (selector, _) in selectors {
        steps.push(json!({"styles": selector, "props": ["z-index", "background-color"]}));
    }
    steps.push(json!({"eval": "layers", "js": LAYERS}));
    steps
}

#[test]
fn the_update_dialog_its_scrim_and_the_menus_sit_on_their_layer_tokens() {
    let Some(chromium) = web_chromium::binary() else { return };
    let desk: [(&str, &str); 4] = [(".sidebar .project-list", "--z-menu"), (".run-more-menu", "--z-menu"), (".update-scrim", "--z-scrim"), (".update-card", "--z-dialog")];
    let steps = layer_steps(vec![json!({"wait": ".update-card"}), json!({"click": "[data-run-more]"}), json!({"tokens": ["--scrim"]})], &desk);
    let result = load(&chromium, "dark", 1280, "/projects/p01/orchestrations", 1, true, steps);
    assert_layers(&result, &desk, "update dialog");
    assert_eq!(style(&result, ".update-scrim", "background-color"), result["tokens"]["--scrim"].as_str().unwrap(), "the update scrim is the scrim token");
}

#[test]
fn the_update_button_sits_on_its_layer_token() {
    let Some(chromium) = web_chromium::binary() else { return };
    let fab: [(&str, &str); 1] = [(".update-fab", "--z-fab")];
    let steps = layer_steps(vec![json!({"wait": ".update-card"}), json!({"click": "[data-update-later]"}), json!({"wait": ".update-fab"})], &fab);
    assert_layers(&load(&chromium, "light", 1280, "/projects/p01/orchestrations", 1, true, steps), &fab, "update button");
}

#[test]
fn the_floating_question_sits_on_the_menu_layer() {
    let Some(chromium) = web_chromium::binary() else { return };
    let float: [(&str, &str); 1] = [("[data-run-action-float]", "--z-menu")];
    let steps = layer_steps(vec![json!({"wait": "td [data-resume]"}), json!({"click": "td [data-resume]"}), json!({"wait": "[data-run-action-float] .confirm"})], &float);
    assert_layers(&load(&chromium, "light", 1280, "/projects/p01/orchestrations", 1, false, steps), &float, "floating question");
}

#[test]
fn the_feature_modal_sits_on_the_dialog_layer() {
    let Some(chromium) = web_chromium::binary() else { return };
    let modal: [(&str, &str); 1] = [(".feature-modal", "--z-dialog")];
    let steps = layer_steps(vec![json!({"click": "[data-dashboard-add]"}), json!({"wait": ".feature-modal"})], &modal);
    assert_layers(&load(&chromium, "light", 1280, "/dashboard", 2, false, steps), &modal, "feature modal");
}

#[test]
fn the_phone_more_sheet_and_its_backdrop_sit_on_their_layer_tokens() {
    let Some(chromium) = web_chromium::binary() else { return };
    // Opened, the tab bar is the More sheet, a sheet above its scrim.
    let phone: [(&str, &str); 4] = [(".topbar", "--z-chrome"), (".tabbar-backdrop", "--z-scrim"), (".project-picker .project-list", "--z-menu"), (".tabbar[data-open]", "--z-dialog")];
    let phone = &phone[..];
    let steps = layer_steps(vec![json!({"wait": "[data-tab-handle]"}), json!({"click": "[data-tab-handle]"}), json!({"wait": ".tabbar-backdrop"}), json!({"tokens": ["--scrim"]})], phone);
    let result = load(&chromium, "dark", 390, "/projects/p01/features", 2, false, steps);
    assert_layers(&result, phone, "More sheet");
    assert_eq!(style(&result, ".tabbar-backdrop", "background-color"), result["tokens"]["--scrim"].as_str().unwrap(), "the More sheet's backdrop is the scrim token");
}

#[test]
fn the_phone_bars_and_the_selection_sheet_sit_on_their_layer_tokens() {
    let Some(chromium) = web_chromium::binary() else { return };
    let chrome: [(&str, &str); 2] = [(".topbar", "--z-chrome"), (".tabbar", "--z-chrome")];
    let steps = layer_steps(vec![json!({"wait": ".tabbar"})], &chrome);
    assert_layers(&load(&chromium, "light", 390, "/projects/p01/features", 2, false, steps), &chrome, "page chrome");

    let marked: [(&str, &str); 1] = [(".marked-actions", "--z-sheet")];
    let steps = layer_steps(vec![json!({"click": "[data-select-mode]"}), json!({"click": "[data-mark=\"f1\"]"}), json!({"wait": "[data-queue-marked]"})], &marked);
    assert_layers(&load(&chromium, "light", 390, "/projects/p01/features", 2, false, steps), &marked, "selection sheet");
}

#[test]
fn the_shadow_tokens_are_dark_with_a_border_outline_in_both_themes() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    for token in ["--scrim:", "--shadow-menu:", "--shadow-dialog:", "--z-chrome: 10;", "--z-sheet: 20;", "--z-menu: 30;", "--z-scrim: 40;", "--z-dialog: 50;", "--z-fab: 60;"] {
        assert_eq!(css.matches(token).count(), 4, "{token} in every theme block");
    }
    assert_eq!(css.matches("--scrim: rgb(15 23 42 / 0.45);").count(), 2, "light themes");
    assert_eq!(css.matches("--scrim: rgb(0 0 0 / 0.6);").count(), 2, "dark themes");
    for line in css.lines().filter(|l| l.trim_start().starts_with("--shadow-")) {
        assert!(line.contains("0 0 0 1px var(--border)"), "a 1px border outline: {line}");
        assert!(!line.contains("var(--text)"), "built on a dark colour, not the text colour: {line}");
    }
    for line in css.lines().filter(|l| l.contains("z-index:")) {
        let value = line.split("z-index:").nth(1).unwrap().trim_start();
        let overlay = [".topbar", ".tabbar", ".marked-actions", ".project-list", ".dropdown-list", ".run-more-menu", ".run-action-float", ".update-", ".feature-modal", ".shell::before", ".design-zoom"].iter().any(|s| line.contains(s));
        if overlay {
            assert!(value.starts_with("var(--z-"), "an overlay keeps no number of its own: {line}");
        }
    }
    for selector in [".run-more-menu {", ".run-action-float .confirm {", ".dropdown-list {"] {
        let line = css.lines().find(|l| l.starts_with(selector)).unwrap_or_else(|| panic!("{selector} rule"));
        assert!(line.contains("box-shadow: var(--shadow-menu)"), "{line}");
    }
    for selector in [".update-card {", ".update-fab {", ".feature-modal-box {"] {
        let line = css.lines().find(|l| l.starts_with(selector)).unwrap_or_else(|| panic!("{selector} rule"));
        assert!(line.contains("box-shadow: var(--shadow-dialog)"), "{line}");
    }
}

fn dropdown_js(list: &str) -> String {
    format!(
        "var list = document.querySelector('{list}'); var s = getComputedStyle(list);
return {{overflowY: s.overflowY, classes: list.className, scrollHeight: list.scrollHeight, clientHeight: list.clientHeight, scrollTop: list.scrollTop, pageScroll: window.scrollY}};"
    )
}

struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

fn rect(result: &Value, selector: &str) -> Rect {
    let b = &result["boxes"][selector][0];
    let n = |k: &str| b[k].as_f64().unwrap_or_else(|| panic!("no box for {selector}: {result}"));
    Rect { left: n("left"), top: n("top"), right: n("right"), bottom: n("bottom") }
}

fn assert_dropdown(chromium: &Path, width: u32, count: usize, open: &str, button: &str, list: &str, at: &str) -> Value {
    let id = format!("p{count:02}");
    let row = format!("{list} [aria-selected=\"true\"]");
    let steps = vec![json!({"wait": open}), json!({"click": open}), json!({"wait": format!("{list}:not([hidden])")}), json!({"boxes": button}), json!({"boxes": list}), json!({"boxes": row}), json!({"eval": "list", "js": dropdown_js(list)})];
    let result = load(chromium, "dark", width, &format!("/projects/{id}/features"), count, false, steps);
    let (b, l, r) = (rect(&result, button), rect(&result, list), rect(&result, &row));
    let facts = &result["evals"]["list"];
    assert!(facts["classes"].as_str().unwrap().split(' ').any(|c| c == "dropdown-list"), "{at}: the shared dropdown style: {result}");
    assert!(l.right - l.left >= b.right - b.left - 0.5, "{at}: the list ({}) is at least as wide as its button ({}): {result}", l.right - l.left, b.right - b.left);
    assert!(l.bottom - l.top <= 0.6 * 800.0 + 1.0, "{at}: at most 60vh tall: {result}");
    assert!(r.top >= l.top && r.bottom <= l.bottom && r.left >= l.left && r.right <= l.right, "{at}: the selected row is fully visible in the list: {result}");
    assert!(r.top >= 0.0 && r.bottom <= 800.0, "{at}: the selected row is in the window: {result}");
    assert_eq!(facts["overflowY"], "auto", "{at}: it scrolls inside itself: {result}");
    assert_eq!(facts["pageScroll"], 0, "{at}: the page itself did not scroll: {result}");
    result
}

#[test]
fn the_sidebar_project_list_is_wide_tall_enough_and_shows_the_chosen_project() {
    let Some(chromium) = web_chromium::binary() else { return };
    let (open, list) = ("[data-project-switcher]", "[data-project-list]");
    let result = assert_dropdown(&chromium, 1280, 12, open, open, list, "sidebar, 12 projects");
    let facts = &result["evals"]["list"];
    assert!(facts["scrollHeight"].as_f64() <= Some(facts["clientHeight"].as_f64().unwrap() + 1.0), "12 projects fit without a scroll: {result}");
    let result = assert_dropdown(&chromium, 1280, 30, open, open, list, "sidebar, 30 projects");
    let facts = &result["evals"]["list"];
    assert!(facts["scrollHeight"].as_f64() > facts["clientHeight"].as_f64(), "30 projects scroll inside the list: {result}");
    assert!(facts["scrollTop"].as_f64().unwrap() > 0.0, "scrolled to the chosen last project: {result}");
}

#[test]
fn the_page_project_picker_list_is_wide_tall_enough_and_shows_the_chosen_project() {
    let Some(chromium) = web_chromium::binary() else { return };
    let (open, list) = ("[data-project-select]", ".project-picker .project-list");
    for width in [1280, 390] {
        assert_dropdown(&chromium, width, 12, open, open, list, &format!("page picker {width}, 12 projects"));
        let result = assert_dropdown(&chromium, width, 30, open, open, list, &format!("page picker {width}, 30 projects"));
        let facts = &result["evals"]["list"];
        assert!(facts["scrollHeight"].as_f64() > facts["clientHeight"].as_f64(), "{width}: 30 projects scroll inside the list: {result}");
        assert!(facts["scrollTop"].as_f64().unwrap() > 0.0, "{width}: scrolled to the chosen last project: {result}");
    }
}

#[test]
fn on_a_phone_the_page_picker_list_stays_inside_the_window_with_a_long_project_name() {
    let Some(chromium) = web_chromium::binary() else { return };
    let long = "a-project-with-a-very-long-name-that-would-run-past-the-right-edge-of-a-phone";
    let mut named = projects(2);
    named["projects"][1]["name"] = json!(long);
    let mut all = replies(2);
    all[0] = web_chromium::get("/api/projects", named);
    let (open, list) = ("[data-project-select]", ".project-picker .project-list");
    let steps = json!([{"wait": open}, {"click": open}, {"wait": format!("{list}:not([hidden])")}, {"boxes": list}, {"eval": "window", "js": "return {width: document.documentElement.clientWidth};"}]);
    let measured = web_chromium::measure_with(&chromium, 390, "/projects/p01/features", &all, steps, "try { localStorage.setItem(\"orq.theme\", \"dark\"); } catch (e) {}");
    let result = measured.result;
    assert!(result.get("error").is_none(), "{result} (unanswered {:?})", measured.unanswered);
    let l = rect(&result, list);
    let width = result["evals"]["window"]["width"].as_f64().unwrap();
    assert!(l.left >= 0.0 && l.right <= width + 0.5, "the list ({} to {}) stays inside the {width}px window: {result}", l.left, l.right);
}
