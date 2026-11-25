//! On a phone the page is framed like a mobile app and there is no sidebar: a fixed top bar with the
//! theme switch, the centred brand and the page's Refresh (signing out is on Settings), and a fixed bottom tab bar holding the destinations,
//! coloured like the reference sidebar with the current tab as the same light pill. The bar has two rows of icon tabs on
//! the same five columns: Home, Features, Orchestrations, Queue and Settings always show, under a short grip handle;
//! Todos, Reviews and Ask orq (a toggle for the Ask orq sheet, not a link) are a second row the handle slides in and out
//! by a tap or a drag. It starts open where its marked tab must be in sight, and is remembered per browser.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn phone(css: &str) -> &str {
    let start = css.find("@media (max-width: 700px) {").unwrap();
    &css[start..start + css[start..].find("\n}\n").unwrap()]
}

fn rule<'a>(block: &'a str, selector: &str) -> &'a str {
    let start = block.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &block[start..start + block[start..].find('}').unwrap()]
}

fn shell(scenario: Value) -> Option<Value> {
    web_deps::ready().then(|| {
        let dir = env!("CARGO_MANIFEST_DIR");
        let output = Command::new("node").arg(format!("{dir}/tests/web_shell_run.mjs")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

const TABS: &str = "nav.tabbar a, nav.tabbar [data-ask-orq]";
const FIRST: &str = "nav.tabbar > [data-tab-row=\"1\"] > a";
const SECOND: &str = "nav.tabbar > [data-tab-row=\"2\"] > :is(a, button)";
const LABELS: &str = "nav.tabbar :is(a, [data-ask-orq]) .tab-label";
/// The `dots` icon's path, which the old More tab showed.
const DOTS: &str = "M5 12h.010M12 12h.010M19 12h.010";

fn probed<'a>(frame: &'a Value, selector: &str) -> &'a Vec<Value> {
    frame["probes"][selector].as_array().unwrap_or_else(|| panic!("no probe for {selector}: {frame}"))
}

fn texts(frame: &Value, selector: &str) -> Vec<String> {
    probed(frame, selector).iter().map(|item| item["text"].as_str().unwrap().to_string()).collect()
}

fn on_phone(path: &str, steps: Value) -> Option<Value> {
    shell(json!({"path": path, "projectId": "demo", "phone": true, "probe": [TABS, FIRST, SECOND, LABELS], "steps": steps}))
}

fn current(frame: &Value, selector: &str) -> Vec<String> {
    probed(frame, selector).iter().filter(|item| item["current"] == "page").map(|item| item["text"].as_str().unwrap().to_string()).collect()
}

const HANDLE: &str = "nav.tabbar > [data-tab-handle]";
const ROW2: &str = "nav.tabbar > [data-tab-row=\"2\"]";

fn row2_open(frame: &Value) -> bool {
    let row = probed(frame, ROW2);
    assert_eq!(row.len(), 1, "one second row: {frame}");
    row[0]["hidden"].is_null()
}

fn handle(frame: &Value) -> &Value {
    let handle = probed(frame, HANDLE);
    assert_eq!(handle.len(), 1, "one handle: {frame}");
    &handle[0]
}

/// The phone shell at `path` with the handle and the second row probed, playing `steps`.
fn bar(path: &str, steps: Value) -> Option<Value> {
    let unfocusable = "nav.tabbar > [data-tab-row=\"2\"] > :is(a, button)[tabindex=\"-1\"]";
    shell(json!({"path": path, "projectId": "demo", "phone": true, "probe": [FIRST, SECOND, HANDLE, ROW2], "selectors": [unfocusable], "steps": steps}))
}

fn unfocusable(frame: &Value) -> i64 {
    frame["matches"]["nav.tabbar > [data-tab-row=\"2\"] > :is(a, button)[tabindex=\"-1\"]"].as_i64().unwrap()
}

#[test]
fn closed_the_bar_shows_the_first_row_and_the_handle_and_hides_the_second_row() {
    let Some(out) = bar("/projects/demo/features", json!([])) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, FIRST), ["Dashboard", "Features", "Orchestrations", "Queue", "Settings"], "{out}");
    assert_eq!(handle(frame)["label"], "More sections", "{out}");
    assert_eq!(handle(frame)["expanded"], "false", "{out}");
    assert!(!row2_open(frame), "the second row is hidden: {out}");
    assert_eq!(probed(frame, ROW2)[0]["height"], "0px", "{out}");
    assert_eq!(unfocusable(frame), 3, "its tabs are out of the tab order: {out}");
}

#[test]
fn tapping_the_handle_opens_the_second_row_and_tapping_again_closes_it() {
    let Some(out) = bar("/projects/demo/features", json!([{"click": HANDLE}, {"click": HANDLE}])) else { return };
    let open = &out["frames"][1];
    assert!(row2_open(open), "{out}");
    assert_eq!(handle(open)["expanded"], "true", "{out}");
    assert_eq!(texts(open, SECOND), ["Todos", "Reviews", "Ask orq"], "{out}");
    assert_eq!(probed(open, ROW2)[0]["height"], "300px", "{out}");
    assert_eq!(unfocusable(open), 0, "its tabs are in the tab order: {out}");
    let closed = &out["frames"][2];
    assert!(!row2_open(closed), "{out}");
    assert_eq!(handle(closed)["expanded"], "false", "{out}");
    assert_eq!(probed(closed, ROW2)[0]["height"], "0px", "{out}");
}

/// A finger pressing the handle at `from`, moving to `to` and lifting there (the second row measures 300px).
fn drag(from: i64, to: i64) -> Vec<Value> {
    vec![json!({"pointer": "down", "at": HANDLE, "y": from}), json!({"pointer": "move", "at": HANDLE, "y": to}), json!({"pointer": "up", "at": HANDLE, "y": to, "thenClick": true})]
}

#[test]
fn a_drag_follows_the_finger_and_opens_past_half_way() {
    let Some(out) = bar("/projects/demo/features", Value::Array(drag(600, 420))) else { return };
    let moving = &probed(&out["frames"][2], ROW2)[0];
    assert_eq!((moving["height"].as_str(), moving["transition"].as_str()), (Some("180px"), Some("none")), "it follows the finger: {out}");
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(row2_open(last), "60% opens it, and the drag's own click does not toggle it back: {out}");
    assert_eq!(probed(last, ROW2)[0]["height"], "300px", "{out}");
    assert_eq!(handle(last)["expanded"], "true", "{out}");
}

#[test]
fn a_drag_short_of_half_way_snaps_back() {
    let Some(out) = bar("/projects/demo/features", Value::Array(drag(600, 500))) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(!row2_open(last), "{out}");
    assert_eq!(probed(last, ROW2)[0]["height"], "0px", "{out}");
}

#[test]
fn a_drag_down_closes_the_open_row() {
    let mut steps = vec![json!({"click": HANDLE})];
    steps.extend(drag(300, 480));
    let Some(out) = bar("/projects/demo/features", Value::Array(steps)) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(!row2_open(last), "{out}");
    assert_eq!(probed(last, ROW2)[0]["height"], "0px", "{out}");
}

#[test]
fn on_a_page_whose_section_is_in_the_second_row_it_starts_open() {
    for path in ["/todos", "/projects/demo/reviews"] {
        let Some(out) = bar(path, json!([])) else { return };
        let frame = &out["frames"][0];
        assert!(row2_open(frame), "{path}: {out}");
        assert_eq!(handle(frame)["expanded"], "true", "{path}: {out}");
        assert_eq!(current(frame, SECOND).len(), 1, "the marked tab is in sight: {out}");
    }
}

#[test]
fn choosing_reviews_in_the_second_row_goes_there_and_closes_it() {
    let Some(out) = bar("/projects/demo/features", json!([{"click": HANDLE}, {"click": "nav.tabbar a[href=\"/projects/demo/reviews\"]"}])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(row2_open(&frames[1]), "{out}");
    assert_eq!(frames[2]["path"], "/projects/demo/reviews", "{out}");
    assert!(!row2_open(&frames[2]), "{out}");
    assert_eq!(current(&frames[2], SECOND), ["Reviews"], "{out}");
}

#[test]
fn going_back_to_a_page_in_the_second_row_opens_it() {
    let Some(out) = bar("/projects/demo/features", json!([{"go": "/todos"}])) else { return };
    assert!(row2_open(&out["frames"][1]), "{out}");
}

#[test]
fn the_first_row_is_home_features_orchestrations_queue_and_settings_and_the_second_is_todos_reviews_and_ask_orq() {
    let Some(out) = on_phone("/projects/demo/features", json!([])) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, FIRST), ["Dashboard", "Features", "Orchestrations", "Queue", "Settings"], "{out}");
    assert_eq!(texts(frame, SECOND), ["Todos", "Reviews", "Ask orq"], "{out}");
    assert!(probed(frame, TABS).iter().all(|item| item["icon"].is_string() && item["iconSize"] == "28"), "every tab has its icon: {out}");
    assert_eq!(current(frame, TABS), ["Features"], "{out}");
}

#[test]
fn the_first_tab_is_home_with_the_home_icon() {
    let Some(home) = shell(json!({"tabbar": [{"label": "Dashboard", "icon": "home", "href": "/dashboard"}], "path": "/dashboard", "probe": [TABS]})) else { return };
    let Some(out) = on_phone("/dashboard", json!([])) else { return };
    let frame = &out["frames"][0];
    assert_eq!(probed(frame, FIRST)[0]["icon"], probed(&home["frames"][0], TABS)[0]["icon"], "the home icon: {out}");
    assert_eq!(current(frame, FIRST), ["Dashboard"], "{out}");
}

#[test]
fn the_current_section_in_the_second_row_is_the_light_pill_there() {
    let Some(out) = on_phone("/projects/demo/reviews", json!([])) else { return };
    let frame = &out["frames"][0];
    assert!(current(frame, FIRST).is_empty(), "{out}");
    assert_eq!(current(frame, SECOND), ["Reviews"], "{out}");
    let Some(out) = on_phone("/todos", json!([])) else { return };
    assert_eq!(current(&out["frames"][0], SECOND), ["Todos"], "{out}");
}

#[test]
fn tapping_a_tab_in_either_row_goes_there() {
    let steps = json!([{"click": "nav.tabbar a[href=\"/todos\"]"}, {"click": "nav.tabbar a[href=\"/settings\"]"}]);
    let Some(out) = on_phone("/projects/demo/features", steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(frames[1]["path"], "/todos", "{out}");
    assert_eq!(current(&frames[1], SECOND), ["Todos"], "{out}");
    assert_eq!(frames[2]["path"], "/settings", "{out}");
    assert_eq!(current(&frames[2], FIRST), ["Settings"], "{out}");
}

#[test]
fn without_a_project_the_rows_hold_the_same_sections() {
    let Some(out) = shell(json!({"path": "/dashboard", "phone": true, "probe": [FIRST, SECOND]})) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, FIRST), ["Dashboard", "Features", "Orchestrations", "Queue", "Settings"], "{out}");
    assert_eq!(texts(frame, SECOND), ["Todos", "Reviews", "Ask orq"], "{out}");
}

#[test]
fn the_tab_bar_has_no_backdrop_or_more_tab() {
    let gone = ["[data-tab-backdrop]", ".tabbar-all", ".tabbar-backdrop", "nav.tabbar button:not([data-ask-orq]):not([data-tab-handle])", "[data-tab-more]", ".more-sheet"];
    let Some(out) = shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [TABS], "selectors": gone})) else { return };
    let frame = &out["frames"][0];
    for selector in gone {
        assert_eq!(frame["matches"][selector], 0, "{selector}: {out}");
    }
    assert!(probed(frame, TABS).iter().all(|item| item["icon"] != DOTS), "no dots icon: {out}");
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/shell.jsx")).unwrap();
    for dead in ["BAR_TABS", "overflows", "data-tab-backdrop", "name=\"dots\""] {
        assert!(!source.contains(dead), "{dead} is no longer in the shell");
    }
}

#[test]
fn the_bar_holds_each_section_once_as_icons_with_hidden_names() {
    let Some(out) = on_phone("/projects/demo/features", json!([])) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, TABS), ["Dashboard", "Features", "Orchestrations", "Queue", "Settings", "Todos", "Reviews", "Ask orq"], "each section once: {out}");
    assert_eq!(texts(frame, LABELS), texts(frame, TABS), "every name is a visually hidden label: {out}");
    assert_eq!(probed(frame, TABS).len(), probed(frame, FIRST).len() + probed(frame, SECOND).len(), "every tab is in a row: {out}");
}

#[test]
fn a_phone_has_no_sidebar_or_menu_and_a_header_with_theme_and_brand_but_no_sign_out() {
    let selectors = [".sidebar", "[data-menu-button]", "[data-scrim]", ".topbar > .theme-toggle", ".topbar > .brand svg", ".topbar > .sign-out"];
    let Some(out) = shell(json!({"path": "/dashboard", "phone": true, "selectors": selectors})) else { return };
    let frame = &out["frames"][0];
    for gone in [".sidebar", "[data-menu-button]", "[data-scrim]", ".topbar > .sign-out"] {
        assert_eq!(frame["matches"][gone], 0, "{gone}: {out}");
    }
    for here in [".topbar > .theme-toggle", ".topbar > .brand svg"] {
        assert_eq!(frame["matches"][here], 1, "{here}: {out}");
    }
    assert!(frame["topbarText"].as_str().unwrap().contains("Orq"), "the app name is in the header: {out}");
}

#[test]
fn a_desktop_has_the_sidebar_and_no_tab_bar_and_resizing_swaps_them() {
    let selectors = [".sidebar", ".tabbar", ".topbar"];
    let Some(out) = shell(json!({"path": "/dashboard", "phone": false, "selectors": selectors, "steps": [{"viewport": "phone"}, {"viewport": "desktop"}]})) else { return };
    let seen = |frame: usize, key: &str| out["frames"][frame]["matches"][key].as_i64().unwrap();
    assert_eq!((seen(0, ".sidebar"), seen(0, ".tabbar"), seen(0, ".topbar")), (1, 0, 0), "{out}");
    assert_eq!((seen(1, ".sidebar"), seen(1, ".tabbar"), seen(1, ".topbar")), (0, 1, 1), "{out}");
    assert_eq!((seen(2, ".sidebar"), seen(2, ".tabbar"), seen(2, ".topbar")), (1, 0, 0), "{out}");
}

#[test]
fn the_tab_bar_is_fixed_to_the_bottom_on_a_phone_and_absent_elsewhere() {
    let css = css();
    let desktop = &css[..css.find("@media (max-width: 700px) {").unwrap()];
    assert!(rule(desktop, ".tabbar {").contains("display: none"), "hidden above phone width");
    let phone = phone(&css);
    let bar = rule(phone, ".tabbar {");
    assert!(bar.contains("display: flex") && bar.contains("flex-direction: column") && bar.contains("position: fixed") && bar.contains("bottom: 0") && bar.contains("left: 0") && bar.contains("right: 0"), "{bar}");
    assert!(bar.contains("env(safe-area-inset-bottom)"), "clears the home indicator: {bar}");
    assert!(bar.contains("padding: 0.375rem 0.5rem") && bar.contains("padding-top: 1.125rem"), "the tabs are spaced like the sidebar items, under the grip: {bar}");
    let tab = rule(phone, ".tabbar a {");
    assert!(tab.contains("flex-direction: column") && tab.contains("min-height: 48px") && tab.contains("border-radius: 0.75rem"), "{tab}");
}

#[test]
fn the_tabs_show_only_larger_icons_and_keep_their_names_for_screen_readers() {
    let css = css();
    let label = rule(phone(&css), ".tabbar .tab-label {");
    assert!(label.contains("position: absolute") && label.contains("clip: rect(0 0 0 0)"), "the text is visually hidden, not removed: {label}");
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/shell.jsx")).unwrap();
    assert!(source.contains("<Icon name={entry.icon} size={28} />"), "the tab icons are larger");
}

#[test]
fn the_bars_use_the_sidebar_colours_and_the_current_tab_in_either_row_is_the_light_pill() {
    let css = css();
    let both = rule(&css, ".topbar, .tabbar {");
    assert!(both.contains("background: var(--sidebar)") && both.contains("color: var(--sidebar-text)"), "{both}");
    let tab = rule(phone(&css), ".tabbar a {");
    assert!(tab.contains("color: var(--sidebar-muted)"), "{tab}");
    let current = rule(phone(&css), ".tabbar a[aria-current=\"page\"] {");
    assert!(current.contains("background: var(--sidebar-active-bg)") && current.contains("color: var(--sidebar-active-text)"), "{current}");
}

#[test]
fn the_two_rows_use_a_five_column_grid() {
    let css = css();
    let phone = phone(&css);
    let row = rule(phone, ".tabbar-row {");
    assert!(row.contains("display: grid") && row.contains("grid-template-columns: repeat(5, 1fr)") && row.contains("gap: 0.25rem"), "{row}");
    assert!(!css.contains("@media (min-width: 600px) and (max-width: 700px)"), "one column template at every phone width");
    let grip = rule(phone, ".tabbar-handle::before {");
    assert!(grip.contains("width: 36px") && grip.contains("height: 4px"), "a short grip: {grip}");
    assert!(rule(phone, ".tabbar-more {").contains("overflow: hidden"), "the closed second row shows nothing");
    for dead in [".tabbar-all", ".tabbar-backdrop", ".tabbar[data-handle]", ".tabbar[data-open]", ".tabbar-grid", "tile-label", ".more-sheet", "data-tab-more"] {
        assert!(!css.contains(dead), "{dead} is no longer used");
    }
}

#[test]
fn the_top_bar_is_fixed_and_the_content_clears_both_bars() {
    let css = css();
    let phone = phone(&css);
    let top = rule(phone, ".topbar {");
    assert!(top.contains("position: sticky") && top.contains("top: 0"), "{top}");
    assert!(top.contains("display: grid") && top.contains("grid-template-columns: 1fr auto 1fr"), "theme, brand, refresh: {top}");
    assert!(rule(phone, ".topbar .brand {").contains("justify-self: center"), "the app icon and name sit in the centre of the header");
    assert!(rule(phone, ".content {").contains("padding-bottom: calc(var(--tabbar-height, 8.5rem + env(safe-area-inset-bottom)) + 0.75rem)"), "content is not hidden under the tab bar, at its measured height");
}

mod web_chromium;

/// Headless chromium's virtual time does not run CSS transitions, so the browser tests ask for reduced motion:
/// the second row then opens and closes at once.
const SETTLED: &str = "(function () { var query = window.matchMedia.bind(window); window.matchMedia = function (media) { \
    return media.indexOf('prefers-reduced-motion') >= 0 ? { media: media, matches: true, addEventListener: function () {}, removeEventListener: function () {} } : query(media); }; })();";

fn edge(result: &Value, selector: &str, at: usize, side: &str) -> i64 {
    result["boxes"][selector][at][side].as_i64().unwrap_or_else(|| panic!("no {side} of {selector} #{at}: {result}"))
}

#[test]
fn in_a_browser_the_second_row_sits_under_the_first_on_its_columns() {
    let Some(chromium) = web_chromium::binary() else { return };
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = [web_chromium::get("/api/projects", json!({"projects": [project]}))];
    for width in [390, 650] {
        let mut steps = vec![json!({"wait": "nav.tabbar a"}), json!({"click": HANDLE}), json!({"wait": "nav.tabbar[data-open]"})];
        for selector in ["nav.tabbar", FIRST, SECOND] {
            steps.push(json!({"boxes": selector}));
        }
        let result = web_chromium::measure_with(&chromium, width, "/dashboard", &replies, Value::Array(steps), SETTLED).result;
        assert!(result.get("error").is_none(), "{result}\n=> {width}: {}", result["error"]);
        let boxes = result["boxes"].to_string();
        let same = |a: i64, b: i64, what: &str| assert!(a == b, "{boxes}\n=> {width}: {what}: {a} != {b}");
        same(result["boxes"][FIRST].as_array().unwrap().len() as i64, 5, "five tabs in the first row");
        same(result["boxes"][SECOND].as_array().unwrap().len() as i64, 3, "Todos, Reviews and Ask orq in the second row");
        for at in 0..5 {
            same(edge(&result, FIRST, at, "top"), edge(&result, FIRST, 0, "top"), "the first row is one row");
            same(edge(&result, FIRST, at, "bottom") - edge(&result, FIRST, at, "top"), 48, "the tabs keep 48px");
        }
        for at in 0..3 {
            same(edge(&result, SECOND, at, "left"), edge(&result, FIRST, at, "left"), "the second row's tab is on the first row's column");
            same(edge(&result, SECOND, at, "right"), edge(&result, FIRST, at, "right"), "and as wide");
            same(edge(&result, SECOND, at, "bottom") - edge(&result, SECOND, at, "top"), 48, "the tabs keep 48px");
            assert!(edge(&result, SECOND, at, "top") >= edge(&result, FIRST, 0, "bottom"), "{boxes}\n=> {width}: the second row is under the first");
        }
        assert!(edge(&result, "nav.tabbar", 0, "top") <= edge(&result, FIRST, 0, "top"), "{boxes}\n=> {width}: both rows are inside the bar");
        assert!(edge(&result, SECOND, 0, "bottom") <= edge(&result, "nav.tabbar", 0, "bottom"), "{boxes}\n=> {width}: both rows are inside the bar");
    }
}

fn projects() -> Vec<web_chromium::Reply> {
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    vec![web_chromium::get("/api/projects", json!({"projects": [project]}))]
}

const EXPANDED: &str = "return document.querySelector('[data-tab-handle]').getAttribute('aria-expanded');";

#[test]
fn in_a_browser_opening_ask_orq_from_the_second_row_keeps_it_open() {
    let Some(chromium) = web_chromium::binary() else { return };
    let ask = "nav.tabbar [data-ask-orq]";
    let steps = json!([{"wait": "nav.tabbar a"}, {"click": HANDLE}, {"click": ask}, {"wait": "[data-ask-panel]"}, {"eval": "expanded", "js": EXPANDED}]);
    let result = web_chromium::measure_with(&chromium, 390, "/dashboard", &projects(), steps, SETTLED).result;
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["evals"]["expanded"], "true", "the open panel keeps the second row open: {result}");
}

const STORED: &str = "return localStorage.getItem('orq.tabbar.more');";
/// How far the content's last pixel of padding reaches up from the bottom, against the bar's top.
const CLEARS: &str = "var content = document.querySelector('.content'); var bar = document.querySelector('nav.tabbar').getBoundingClientRect(); \
    return parseFloat(getComputedStyle(content).paddingBottom) >= Math.ceil(bar.height);";

#[test]
fn in_a_browser_open_or_closed_is_remembered_across_a_reload() {
    let Some(chromium) = web_chromium::binary() else { return };
    let steps = json!([{"wait": "nav.tabbar a"}, {"click": HANDLE}, {"eval": "stored", "js": STORED}]);
    let result = web_chromium::measure_with(&chromium, 390, "/dashboard", &projects(), steps, SETTLED).result;
    assert_eq!(result["evals"]["stored"], "open", "opening it is stored: {result}");
    let reloaded = format!("{SETTLED} localStorage.setItem('orq.tabbar.more', 'open');");
    let steps = json!([{"wait": "nav.tabbar a"}, {"eval": "expanded", "js": EXPANDED}, {"eval": "clears", "js": CLEARS}]);
    let result = web_chromium::measure_with(&chromium, 390, "/dashboard", &projects(), steps, &reloaded).result;
    assert_eq!(result["evals"]["expanded"], "true", "it opens as it was left: {result}");
    assert_eq!(result["evals"]["clears"], true, "the content clears the open bar: {result}");
    assert_eq!(result["scrollWidth"], 390, "no horizontal scroll: {result}");
    let steps = json!([{"wait": "nav.tabbar a"}, {"click": HANDLE}, {"eval": "stored", "js": STORED}, {"eval": "expanded", "js": EXPANDED}, {"eval": "clears", "js": CLEARS}]);
    let result = web_chromium::measure_with(&chromium, 390, "/dashboard", &projects(), steps, &reloaded).result;
    assert_eq!((result["evals"]["stored"].as_str(), result["evals"]["expanded"].as_str()), (Some("closed"), Some("false")), "closing it is stored: {result}");
    assert_eq!(result["evals"]["clears"], true, "the content clears the closed bar: {result}");
}

#[test]
fn in_a_browser_the_bar_works_without_storage() {
    let Some(chromium) = web_chromium::binary() else { return };
    let broken = format!("{SETTLED} ['getItem', 'setItem'].forEach(function (name) {{ Object.defineProperty(localStorage, name, {{ value: function () {{ throw new Error('no storage'); }} }}); }});");
    let steps = json!([{"wait": "nav.tabbar a"}, {"click": HANDLE}, {"eval": "expanded", "js": EXPANDED}]);
    let result = web_chromium::measure_with(&chromium, 390, "/dashboard", &projects(), steps, &broken).result;
    assert_eq!(result["evals"]["expanded"], "true", "{result}");
}

#[test]
fn the_drawer_is_gone_from_the_stylesheet() {
    let css = css();
    for dead in [".scrim", "drawer-open", "data-menu-button", ".close-button"] {
        assert!(!css.contains(dead), "{dead} is no longer used");
    }
}
