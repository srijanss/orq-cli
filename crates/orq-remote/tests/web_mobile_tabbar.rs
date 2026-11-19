//! On a phone the page is framed like a mobile app and there is no sidebar: a fixed top bar with the
//! theme switch, the centred brand and the page's Refresh (signing out is on Settings), and a fixed bottom tab bar holding the destinations,
//! coloured like the reference sidebar with the current tab as the same light pill. Past five destinations the bar keeps
//! five tabs and a grab handle on its top edge: dragging it (or tapping it) pulls the bar up to show every section as tiles.

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

const TABS: &str = "nav.tabbar > a";
const HANDLE: &str = "[data-tab-handle]";
const DRAWER: &str = "[data-tab-drawer]";
const TILES: &str = "[data-tab-drawer] a";
const BACKDROP: &str = "[data-tab-backdrop]";
/// The `dots` icon's path, which the old More tab showed.
const DOTS: &str = "M5 12h.010M12 12h.010M19 12h.010";

fn probed<'a>(frame: &'a Value, selector: &str) -> &'a Vec<Value> {
    frame["probes"][selector].as_array().unwrap_or_else(|| panic!("no probe for {selector}: {frame}"))
}

fn texts(frame: &Value, selector: &str) -> Vec<String> {
    probed(frame, selector).iter().map(|item| item["text"].as_str().unwrap().to_string()).collect()
}

/// `count` sections s0, s1, ... for a tab bar on its own.
fn sections(count: usize) -> Value {
    (0..count).map(|at| json!({"label": format!("s{at}"), "icon": "home", "href": format!("/s{at}")})).collect()
}

fn drawer_open(frame: &Value) -> bool {
    let drawer = probed(frame, DRAWER);
    assert_eq!(drawer.len(), 1, "one drawer: {frame}");
    drawer[0]["hidden"].is_null()
}

fn handle(frame: &Value) -> &Value {
    let handle = probed(frame, HANDLE);
    assert_eq!(handle.len(), 1, "one handle: {frame}");
    &handle[0]
}

/// A seven-section bar on /s2 with every probe, playing `steps`.
fn seven(steps: Value, reduced: bool) -> Option<Value> {
    shell(json!({"tabbar": sections(7), "path": "/s2", "reducedMotion": reduced, "probe": [TABS, HANDLE, DRAWER, TILES, BACKDROP], "steps": steps}))
}

/// A finger pressing the handle at 600 and moving to `to`, then resting before it lifts so it is no flick.
fn slow_drag(from: i64, to: i64) -> Value {
    json!([{"pointer": "down", "at": HANDLE, "y": from}, {"pointer": "move", "at": HANDLE, "y": to}, {"wait": 150}, {"pointer": "up", "at": HANDLE, "y": to}])
}

#[test]
fn past_five_sections_the_bar_shows_five_tabs_and_a_handle_and_no_more_tab() {
    let scenario = json!({"tabbar": sections(7), "path": "/s2", "probe": [TABS, HANDLE, DRAWER], "selectors": ["[data-tab-more]", ".more-sheet", "[data-more-sheet]"]});
    let Some(out) = shell(scenario) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, TABS), ["s0", "s1", "s2", "s3", "s4"], "{out}");
    assert_eq!(probed(frame, TABS)[2]["current"], "page", "{out}");
    let grip = handle(frame);
    assert_eq!((grip["tag"].as_str(), grip["label"].as_str(), grip["expanded"].as_str()), (Some("button"), Some("Show all sections"), Some("false")), "{out}");
    for gone in ["[data-tab-more]", ".more-sheet", "[data-more-sheet]"] {
        assert_eq!(frame["matches"][gone], 0, "{gone}: {out}");
    }
    assert!(probed(frame, TABS).iter().chain([grip]).all(|item| item["icon"] != DOTS), "no dots icon: {out}");
    assert!(!drawer_open(frame), "the drawer starts closed: {out}");
    assert_eq!(probed(frame, DRAWER)[0]["height"], "0px", "{out}");
}

#[test]
fn with_a_project_the_bar_has_five_tabs_and_settings_is_behind_the_handle() {
    let Some(out) = shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [TABS, HANDLE, TILES]})) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, TABS), ["Dashboard", "Features", "Orchestrations", "Queue", "Reviews"], "{out}");
    assert_eq!(probed(frame, TABS)[1]["current"], "page", "Features is the current tab: {out}");
    assert!(probed(frame, TABS).iter().all(|item| item["icon"].is_string() && item["iconSize"] == "28"), "every tab has its icon: {out}");
    assert_eq!(texts(frame, TILES), ["Dashboard", "Features", "Orchestrations", "Queue", "Reviews", "Settings"], "{out}");
    assert_eq!(handle(frame)["label"], "Show all sections", "{out}");
}

#[test]
fn a_tap_on_the_handle_opens_every_section_as_tiles_and_a_second_tap_closes_it() {
    let Some(out) = seven(json!([{"click": HANDLE}, {"click": HANDLE}]), false) else { return };
    let open = &out["frames"][1];
    assert!(drawer_open(open), "{out}");
    assert_eq!(texts(open, TILES), ["s0", "s1", "s2", "s3", "s4", "s5", "s6"], "{out}");
    let marked: Vec<Value> = probed(open, TILES).iter().map(|tile| tile["current"].clone()).collect();
    assert_eq!(marked[2], "page", "the current section is marked: {out}");
    assert_eq!(marked.iter().filter(|mark| !mark.is_null()).count(), 1, "{out}");
    assert!(probed(open, TILES).iter().all(|tile| tile["iconSize"] == "28"), "the tiles are the bar's: {out}");
    assert_eq!(handle(open)["expanded"], "true", "{out}");
    assert_eq!(probed(open, DRAWER)[0]["height"], "300px", "{out}");
    assert_eq!(probed(open, BACKDROP).len(), 1, "a backdrop dims the page: {out}");
    assert_eq!(open["active"], json!({"text": "s2", "handle": false, "inDrawer": true}), "focus moves into the grid: {out}");
    let closed = &out["frames"][2];
    assert!(!drawer_open(closed), "{out}");
    assert_eq!(handle(closed)["expanded"], "false", "{out}");
    assert!(probed(closed, BACKDROP).is_empty(), "{out}");
    assert_eq!(closed["active"]["handle"], true, "{out}");
}

#[test]
fn a_finger_tapping_the_handle_opens_and_closes_it_as_a_browser_sends_the_tap() {
    // A real tap: a down and a lift in place, then the click, which goes to whatever captured the finger.
    let Some(out) = seven(json!([{"tap": HANDLE}, {"tap": HANDLE}]), false) else { return };
    let open: Vec<bool> = out["frames"].as_array().unwrap().iter().map(drawer_open).collect();
    assert_eq!(open, [false, true, false], "{out}");
}

#[test]
fn a_drag_and_the_click_after_it_leave_the_drawer_open_and_the_next_tap_still_closes_it() {
    let steps = json!([
        {"pointer": "down", "at": HANDLE, "y": 600}, {"pointer": "move", "at": HANDLE, "y": 480}, {"wait": 150},
        {"pointer": "up", "at": HANDLE, "y": 480, "thenClick": true}, {"wait": 350}, {"tap": HANDLE}
    ]);
    let Some(out) = seven(steps, false) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(drawer_open(&frames[4]), "the drag's own click does not toggle it back: {out}");
    assert!(!drawer_open(frames.last().unwrap()), "a later tap still works: {out}");
}

#[test]
fn a_cancelled_drag_falls_back_and_the_next_tap_still_opens_it() {
    let steps = json!([{"pointer": "down", "at": HANDLE, "y": 600}, {"pointer": "move", "at": HANDLE, "y": 480}, {"pointer": "cancel", "at": HANDLE, "y": 480}, {"wait": 350}, {"tap": HANDLE}]);
    let Some(out) = seven(steps, false) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(!drawer_open(&frames[3]), "{out}");
    assert_eq!(probed(&frames[3], DRAWER)[0]["height"], "0px", "{out}");
    assert!(drawer_open(frames.last().unwrap()), "{out}");
}

#[test]
fn losing_the_finger_mid_drag_falls_back_and_the_next_tap_still_opens_it() {
    let steps = json!([{"pointer": "down", "at": HANDLE, "y": 600}, {"pointer": "move", "at": HANDLE, "y": 480}, {"pointer": "lostcapture", "at": HANDLE, "y": 480}, {"wait": 350}, {"tap": HANDLE}]);
    let Some(out) = seven(steps, false) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(!drawer_open(&frames[3]), "{out}");
    assert_eq!(probed(&frames[3], DRAWER)[0]["height"], "0px", "no drawer is left half pulled: {out}");
    assert!(drawer_open(frames.last().unwrap()), "{out}");
}

#[test]
fn a_touch_drag_from_the_handle_keeps_going_when_the_bar_takes_the_finger() {
    // A touch finger is captured by the handle it lands on; the bar taking it over sends the handle a
    // lostpointercapture that bubbles to the bar. That hand-over is not the finger being lost.
    let steps = json!([
        {"pointer": "down", "at": HANDLE, "y": 600, "touch": true}, {"pointer": "move", "at": HANDLE, "y": 480}, {"wait": 150},
        {"pointer": "up", "at": HANDLE, "y": 480}
    ]);
    let Some(out) = seven(steps, false) else { return };
    assert_eq!(probed(&out["frames"][2], DRAWER)[0]["height"], "120px", "it follows the finger: {out}");
    assert!(drawer_open(out["frames"].as_array().unwrap().last().unwrap()), "40% of the open height opens it: {out}");
}

#[test]
fn a_touch_drag_lifted_as_a_browser_lifts_it_stays_open_and_the_next_tap_still_closes_it() {
    // The whole touch lifecycle in browser order: the handle holds the finger from the press, the bar takes it
    // over once it drags, and the lift sends the bar its pointerup, then its lostpointercapture, then the click.
    let steps = json!([
        {"pointer": "down", "at": HANDLE, "y": 600, "touch": true}, {"pointer": "move", "at": HANDLE, "y": 480}, {"wait": 150},
        {"pointer": "up", "at": HANDLE, "y": 480, "thenClick": true}, {"wait": 350}, {"tap": HANDLE}
    ]);
    let Some(out) = seven(steps, false) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(out["lifted"], json!(["pointerup", "lostpointercapture", "click"]), "the bar gets the lift in browser order: {out}");
    assert!(drawer_open(&frames[4]), "the lift's lost capture and click leave it open: {out}");
    assert!(!drawer_open(frames.last().unwrap()), "a later tap still works: {out}");
}

#[test]
fn an_open_drawer_takes_its_new_height_when_the_window_resizes() {
    // Crossing 600px wide moves the grid from five a row to four, so it grows.
    let Some(out) = seven(json!([{"click": HANDLE}, {"resize": 380}]), false) else { return };
    assert_eq!(probed(&out["frames"][1], DRAWER)[0]["height"], "300px", "{out}");
    assert_eq!(probed(&out["frames"][2], DRAWER)[0]["height"], "380px", "nothing is clipped: {out}");
}

#[test]
fn the_bar_follows_a_drag_and_opens_when_let_go_past_a_third() {
    let Some(out) = seven(slow_drag(600, 480), false) else { return };
    let moving = &probed(&out["frames"][2], DRAWER)[0];
    assert_eq!((moving["height"].as_str(), moving["transition"].as_str()), (Some("120px"), Some("none")), "it follows the finger: {out}");
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(drawer_open(last), "40% of the open height opens it: {out}");
    let settled = &probed(last, DRAWER)[0];
    assert_eq!((settled["height"].as_str(), settled["transition"].as_str()), (Some("300px"), Some("height 200ms ease-out")), "{out}");
}

#[test]
fn a_short_slow_drag_falls_back_closed() {
    let Some(out) = seven(slow_drag(600, 540), false) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(!drawer_open(last), "20% falls back: {out}");
    assert_eq!(probed(last, DRAWER)[0]["height"], "0px", "{out}");
}

#[test]
fn a_fast_short_flick_opens_it() {
    let steps = json!([{"pointer": "down", "at": HANDLE, "y": 600}, {"pointer": "move", "at": HANDLE, "y": 560}, {"pointer": "up", "at": HANDLE, "y": 560}]);
    let Some(out) = seven(steps, false) else { return };
    assert!(drawer_open(out["frames"].as_array().unwrap().last().unwrap()), "{out}");
}

#[test]
fn dragging_the_open_bar_down_closes_it() {
    let mut steps = vec![json!({"click": HANDLE})];
    steps.extend(slow_drag(300, 420).as_array().unwrap().iter().cloned());
    let Some(out) = seven(Value::Array(steps), false) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(!drawer_open(last), "{out}");
    assert_eq!(last["active"]["handle"], true, "{out}");
}

#[test]
fn reduced_motion_neither_follows_the_finger_nor_animates() {
    let Some(out) = seven(slow_drag(600, 480), true) else { return };
    let moving = &probed(&out["frames"][2], DRAWER)[0];
    assert_eq!(moving["height"], "0px", "{out}");
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(drawer_open(last), "{out}");
    assert_eq!(probed(last, DRAWER)[0]["transition"], "none", "{out}");
}

fn phone_shell(steps: Value) -> Option<Value> {
    shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [HANDLE, DRAWER, TILES, BACKDROP], "steps": steps}))
}

fn closed_with_focus_on_the_handle(out: &Value) {
    let frames = out["frames"].as_array().unwrap();
    assert!(drawer_open(&frames[1]), "{out}");
    let last = frames.last().unwrap();
    assert!(!drawer_open(last), "{out}");
    assert!(probed(last, BACKDROP).is_empty(), "{out}");
    assert_eq!(last["active"]["handle"], true, "{out}");
}

#[test]
fn escape_closes_it_and_returns_focus_to_the_handle() {
    let Some(out) = phone_shell(json!([{"click": HANDLE}, {"press": "Escape"}])) else { return };
    closed_with_focus_on_the_handle(&out);
}

#[test]
fn a_tap_on_the_backdrop_closes_it() {
    let Some(out) = phone_shell(json!([{"click": HANDLE}, {"click": BACKDROP}])) else { return };
    closed_with_focus_on_the_handle(&out);
}

#[test]
fn back_closes_it() {
    let Some(out) = phone_shell(json!([{"click": HANDLE}, {"go": "/projects/demo/reviews"}])) else { return };
    closed_with_focus_on_the_handle(&out);
    assert_eq!(out["frames"][2]["path"], "/projects/demo/reviews", "{out}");
}

#[test]
fn choosing_a_section_goes_there_and_closes_it() {
    let Some(out) = phone_shell(json!([{"click": HANDLE}, {"click": "[data-tab-drawer] a[href=\"/settings\"]"}])) else { return };
    closed_with_focus_on_the_handle(&out);
    assert_eq!(out["frames"][2]["path"], "/settings", "{out}");
}

#[test]
fn choosing_the_current_section_closes_it_too() {
    let Some(out) = phone_shell(json!([{"click": HANDLE}, {"click": "[data-tab-drawer] a[href=\"/projects/demo/features\"]"}])) else { return };
    closed_with_focus_on_the_handle(&out);
}

#[test]
fn tab_stays_within_the_bar_while_open() {
    // Open on Features: focus is on its tile, the second, after the handle; past the handle Shift+Tab wraps to the bar's last tab.
    let back = json!({"press": "Tab", "shift": true});
    let Some(out) = phone_shell(json!([{"click": HANDLE}, back, back, back, {"press": "Tab"}])) else { return };
    let focus: Vec<Value> = out["frames"].as_array().unwrap()[1..].iter().map(|frame| frame["active"].clone()).collect();
    assert_eq!(focus[0], json!({"text": "Features", "handle": false, "inDrawer": true}), "{out}");
    assert_eq!(focus[1], json!({"text": "Dashboard", "handle": false, "inDrawer": true}), "{out}");
    assert_eq!(focus[2], json!({"text": "", "handle": true, "inDrawer": false}), "{out}");
    assert_eq!(focus[3], json!({"text": "Reviews", "handle": false, "inDrawer": false}), "wraps to the bar's last tab: {out}");
    assert_eq!(focus[4]["handle"], true, "and Tab wraps back: {out}");
}

#[test]
fn five_destinations_fit_the_bar_and_need_no_handle() {
    let Some(out) = shell(json!({"tabbar": sections(5), "path": "/s4", "probe": [TABS, HANDLE, DRAWER]})) else { return };
    let frame = &out["frames"][0];
    assert_eq!(texts(frame, TABS), ["s0", "s1", "s2", "s3", "s4"], "{out}");
    assert_eq!(probed(frame, TABS)[4]["current"], "page", "{out}");
    assert!(probed(frame, HANDLE).is_empty() && probed(frame, DRAWER).is_empty(), "the bar as it was: {out}");
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
    assert!(bar.contains("display: flex") && bar.contains("position: fixed") && bar.contains("bottom: 0") && bar.contains("left: 0") && bar.contains("right: 0"), "{bar}");
    assert!(bar.contains("env(safe-area-inset-bottom)"), "clears the home indicator: {bar}");
    assert!(bar.contains("gap: 0.25rem") && bar.contains("padding: 0.375rem 0.5rem"), "the tabs are spaced like the sidebar items: {bar}");
    let tab = rule(phone, ".tabbar a {");
    assert!(tab.contains("flex: 1") && tab.contains("flex-direction: column") && tab.contains("min-height: 48px") && tab.contains("border-radius: 0.75rem"), "{tab}");
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
fn the_bars_use_the_sidebar_colours_and_the_current_tab_is_the_light_pill() {
    let css = css();
    let both = rule(&css, ".topbar, .tabbar {");
    assert!(both.contains("background: var(--sidebar)") && both.contains("color: var(--sidebar-text)"), "{both}");
    let tab = rule(phone(&css), ".tabbar a {");
    assert!(tab.contains("color: var(--sidebar-muted)"), "{tab}");
    let current = rule(phone(&css), ".tabbar a[aria-current=\"page\"] {");
    assert!(current.contains("background: var(--sidebar-active-bg)") && current.contains("color: var(--sidebar-active-text)"), "{current}");
}

#[test]
fn the_handle_is_a_short_grip_in_a_44px_strip_and_the_grid_has_four_or_five_columns() {
    let css = css();
    let phone = phone(&css);
    let strip = rule(phone, ".tabbar-handle {");
    for part in ["position: absolute", "height: 2.75rem", "touch-action: none", "border: 0", "background: none"] {
        assert!(strip.contains(part), "{part}: {strip}");
    }
    let grip = rule(phone, ".tabbar-handle::before {");
    for part in ["width: 36px", "height: 4px", "border-radius: 2px", "background: var(--sidebar-muted)"] {
        assert!(grip.contains(part), "{part}: {grip}");
    }
    assert!(!rule(phone, ".tabbar a {").contains("touch-action"), "the tabs still take taps normally");
    assert!(rule(phone, ".tabbar-grid {").contains("grid-template-columns: repeat(4, 1fr)"), "four a row on a phone");
    let wide = &css[css.find("@media (min-width: 600px) and (max-width: 700px) {").expect("a 600px and up block")..];
    assert!(rule(wide, ".tabbar-grid {").contains("grid-template-columns: repeat(5, 1fr)"), "five a row at 600px and up");
    let all = rule(phone, ".tabbar-all {");
    assert!(all.contains("overflow: hidden") && all.contains("flex: 0 0 100%"), "the drawer grows inside the bar: {all}");
    let backdrop = rule(phone, ".tabbar-backdrop {");
    assert!(backdrop.contains("position: fixed") && backdrop.contains("inset: 0"), "{backdrop}");
    for dead in [".more-sheet", "data-tab-more"] {
        assert!(!css.contains(dead), "{dead} is no longer used");
    }
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/shell.jsx")).unwrap();
    assert!(!source.contains("name=\"dots\""), "no dots icon on the bar");
}

#[test]
fn the_top_bar_is_fixed_and_the_content_clears_both_bars() {
    let css = css();
    let phone = phone(&css);
    let top = rule(phone, ".topbar {");
    assert!(top.contains("position: sticky") && top.contains("top: 0"), "{top}");
    assert!(top.contains("display: grid") && top.contains("grid-template-columns: 1fr auto 1fr"), "theme, brand, refresh: {top}");
    assert!(rule(phone, ".topbar .brand {").contains("justify-self: center"), "the app icon and name sit in the centre of the header");
    assert!(rule(phone, ".content {").contains("padding-bottom: calc(5rem + env(safe-area-inset-bottom))"), "content is not hidden under the tab bar");
}

#[test]
fn the_drawer_is_gone_from_the_stylesheet() {
    let css = css();
    for dead in [".scrim", "drawer-open", "data-menu-button", ".close-button"] {
        assert!(!css.contains(dead), "{dead} is no longer used");
    }
}
