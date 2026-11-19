//! On a phone the page is framed like a mobile app and there is no sidebar: a fixed top bar with the
//! theme switch, the centred brand and the page's Refresh (signing out is on Settings), and a fixed bottom tab bar holding the destinations,
//! coloured like the reference sidebar with the current tab as the same light pill.

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

const BAR: &str = "nav.tabbar > *";

fn probed<'a>(frame: &'a Value, selector: &str) -> &'a Vec<Value> {
    frame["probes"][selector].as_array().unwrap_or_else(|| panic!("no probe for {selector}: {frame}"))
}

#[test]
fn with_a_project_the_bar_has_four_links_and_a_more_button() {
    let Some(out) = shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [BAR]})) else { return };
    let items = probed(&out["frames"][0], BAR);
    let shape: Vec<(&str, &str)> = items.iter().map(|item| (item["tag"].as_str().unwrap(), item["text"].as_str().unwrap())).collect();
    assert_eq!(shape, [("a", "Dashboard"), ("a", "Features"), ("a", "Orchestrations"), ("a", "Queue"), ("button", "More")], "{out}");
    let more = &items[4];
    assert_eq!((more["popup"].as_str(), more["expanded"].as_str()), (Some("dialog"), Some("false")), "{out}");
    assert_eq!(more["iconSize"], "28", "{out}");
    assert_eq!(items[1]["current"], "page", "Features is the current tab: {out}");
    assert!(items.iter().all(|item| item["icon"].is_string()), "every tab has its icon: {out}");
}

const SHEET: &str = "[data-more-sheet]";
const SHEET_LINKS: &str = "[data-more-sheet] a";

#[test]
fn more_opens_a_sheet_with_the_other_destinations_and_focuses_its_first() {
    let scenario = json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [BAR, SHEET, SHEET_LINKS], "steps": [{"click": "[data-tab-more]"}]});
    let Some(out) = shell(scenario) else { return };
    assert!(probed(&out["frames"][0], SHEET).is_empty(), "the sheet starts closed: {out}");
    let open = &out["frames"][1];
    let sheet = probed(open, SHEET);
    assert_eq!((sheet.len(), sheet[0]["label"].as_str()), (1, Some("More")), "{out}");
    let links: Vec<(&str, &str)> = probed(open, SHEET_LINKS).iter().map(|link| (link["text"].as_str().unwrap(), link["href"].as_str().unwrap())).collect();
    assert_eq!(links, [("Reviews", "/projects/demo/reviews"), ("Settings", "/settings")], "{out}");
    assert!(probed(open, SHEET_LINKS).iter().all(|link| link["icon"].is_string()), "each has its icon: {out}");
    assert_eq!(probed(open, BAR)[4]["expanded"], "true", "{out}");
    assert_eq!(open["active"], json!({"text": "Reviews", "more": false, "inSheet": true}), "{out}");
}

#[test]
fn escape_closes_the_sheet_and_returns_focus_to_more() {
    let scenario = json!({"path": "/dashboard", "phone": true, "probe": [BAR, SHEET], "steps": [{"click": "[data-tab-more]"}, {"press": "Escape"}]});
    let Some(out) = shell(scenario) else { return };
    let closed = &out["frames"][2];
    assert!(probed(closed, SHEET).is_empty(), "{out}");
    assert_eq!(probed(closed, BAR)[4]["expanded"], "false", "{out}");
    assert_eq!(closed["active"], json!({"text": "More", "more": true, "inSheet": false}), "{out}");
}

#[test]
fn a_press_outside_the_sheet_closes_it_and_more_toggles_it() {
    let steps = [json!({"click": "[data-tab-more]"}), json!({"click": "#content"}), json!({"click": "[data-tab-more]"}), json!({"click": "[data-tab-more]"})];
    let Some(out) = shell(json!({"path": "/dashboard", "phone": true, "probe": [SHEET], "steps": steps})) else { return };
    let open: Vec<bool> = out["frames"].as_array().unwrap().iter().map(|frame| !probed(frame, SHEET).is_empty()).collect();
    assert_eq!(open, [false, true, false, true, false], "{out}");
}

#[test]
fn moving_through_history_closes_the_sheet() {
    let steps = [json!({"click": "[data-tab-more]"}), json!({"go": "/projects/demo/reviews"})];
    let Some(out) = shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [SHEET], "steps": steps})) else { return };
    assert_eq!(probed(&out["frames"][1], SHEET).len(), 1, "{out}");
    let moved = &out["frames"][2];
    assert_eq!(moved["path"], "/projects/demo/reviews", "{out}");
    assert!(probed(moved, SHEET).is_empty(), "Back or Forward leaves no sheet over the new page: {out}");
}

#[test]
fn coming_back_to_the_page_the_sheet_opened_on_leaves_it_closed() {
    let steps = [json!({"click": "[data-tab-more]"}), json!({"go": "/projects/demo/reviews"}), json!({"go": "/projects/demo/features"})];
    let Some(out) = shell(json!({"path": "/projects/demo/features", "projectId": "demo", "phone": true, "probe": [SHEET], "steps": steps})) else { return };
    let open: Vec<bool> = out["frames"].as_array().unwrap().iter().map(|frame| !probed(frame, SHEET).is_empty()).collect();
    assert_eq!(open, [false, true, false, false], "{out}");
}

#[test]
fn picking_a_sheet_destination_goes_there_closes_the_sheet_and_marks_more_current() {
    let scenario = json!({"path": "/dashboard", "phone": true, "probe": [BAR, SHEET, SHEET_LINKS], "steps": [{"click": "[data-tab-more]"}, {"click": "[data-more-sheet] a[href=\"/settings\"]"}, {"click": "[data-tab-more]"}]});
    let Some(out) = shell(scenario) else { return };
    assert_eq!(probed(&out["frames"][0], BAR)[4]["current"], Value::Null, "More is not current on the Dashboard: {out}");
    let gone = &out["frames"][2];
    assert_eq!(gone["path"], "/settings", "{out}");
    assert!(probed(gone, SHEET).is_empty(), "{out}");
    let bar = probed(gone, BAR);
    assert_eq!((bar[0]["current"].clone(), bar[4]["current"].clone()), (Value::Null, json!("page")), "More stands for the page it holds: {out}");
    let reopened: Vec<Value> = probed(&out["frames"][3], SHEET_LINKS).iter().map(|link| link["current"].clone()).collect();
    assert_eq!(reopened, [Value::Null, json!("page")], "Settings is marked inside the sheet: {out}");
}

#[test]
fn five_destinations_fit_the_bar_and_need_no_more_tab() {
    let entries: Vec<Value> = ["a", "b", "c", "d", "e"].iter().map(|name| json!({"label": name, "icon": "home", "href": format!("/{name}")})).collect();
    let Some(out) = shell(json!({"tabbar": entries, "path": "/e", "probe": [BAR]})) else { return };
    let items = probed(&out["frames"][0], BAR);
    let shape: Vec<(&str, &str)> = items.iter().map(|item| (item["tag"].as_str().unwrap(), item["text"].as_str().unwrap())).collect();
    assert_eq!(shape, [("a", "a"), ("a", "b"), ("a", "c"), ("a", "d"), ("a", "e")], "{out}");
    assert_eq!(items[4]["current"], "page", "{out}");
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
fn more_looks_like_a_tab_and_the_sheet_rises_above_the_bar_in_its_colours() {
    let css = css();
    let phone = phone(&css);
    let more = rule(phone, ".tabbar [data-tab-more] {");
    for part in ["flex: 1", "flex-direction: column", "min-height: 48px", "border-radius: 0.75rem", "border: 0", "background: none", "color: var(--sidebar-muted)"] {
        assert!(more.contains(part), "{part}: {more}");
    }
    let current = rule(phone, ".tabbar [data-tab-more][aria-current=\"page\"] {");
    assert!(current.contains("background: var(--sidebar-active-bg)") && current.contains("color: var(--sidebar-active-text)"), "{current}");
    let sheet = rule(phone, ".more-sheet {");
    for part in ["position: fixed", "left: 0", "right: 0", "bottom: calc(", "env(safe-area-inset-bottom)", "background: var(--sidebar)", "color: var(--sidebar-text)", "flex-direction: column"] {
        assert!(sheet.contains(part), "{part}: {sheet}");
    }
    let item = rule(phone, ".more-sheet a {");
    assert!(item.contains("min-height: 48px") && item.contains("color: var(--sidebar-text)"), "{item}");
    let marked = rule(phone, ".more-sheet a[aria-current=\"page\"] {");
    assert!(marked.contains("background: var(--sidebar-active-bg)") && marked.contains("color: var(--sidebar-active-text)"), "{marked}");
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
