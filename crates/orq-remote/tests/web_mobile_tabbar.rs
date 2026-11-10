//! On a phone the page is framed like a mobile app and there is no sidebar: a fixed top bar with the
//! theme switch, the centred brand and sign-out, and a fixed bottom tab bar holding the destinations,
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

#[test]
fn the_tab_bar_holds_the_five_destinations_and_marks_the_current_one() {
    let selectors = ["nav.tabbar a", "nav.tabbar a[aria-current=\"page\"]", "nav.tabbar a svg", "nav.tabbar a .tab-label"];
    let Some(out) = shell(json!({"path": "/dashboard", "phone": true, "selectors": selectors})) else { return };
    let m = &out["frames"][0]["matches"];
    assert_eq!(m["nav.tabbar a"], 5, "{out}");
    assert_eq!(m["nav.tabbar a[aria-current=\"page\"]"], 1, "{out}");
    assert_eq!(m["nav.tabbar a svg"], 5, "{out}");
    assert_eq!(m["nav.tabbar a .tab-label"], 5, "{out}");
}

#[test]
fn a_phone_has_no_sidebar_or_menu_and_a_header_with_theme_brand_and_sign_out() {
    let selectors = [".sidebar", "[data-menu-button]", "[data-scrim]", ".topbar > .theme-toggle", ".topbar > .brand svg", ".topbar > .sign-out"];
    let Some(out) = shell(json!({"path": "/dashboard", "phone": true, "selectors": selectors})) else { return };
    let frame = &out["frames"][0];
    for gone in [".sidebar", "[data-menu-button]", "[data-scrim]"] {
        assert_eq!(frame["matches"][gone], 0, "{gone}: {out}");
    }
    for here in [".topbar > .theme-toggle", ".topbar > .brand svg", ".topbar > .sign-out"] {
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
    assert!(tab.contains("flex: 1") && tab.contains("flex-direction: column") && tab.contains("min-height: 52px") && tab.contains("border-radius: 0.75rem"), "{tab}");
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
    assert!(current.contains("border-radius: 0 0 1.75rem 1.75rem") && current.contains("margin-top: -0.375rem"), "the current tab hangs from the top edge like a drop: {current}");
    let phone = phone(&css);
    assert!(phone.contains(".tabbar a[aria-current=\"page\"]::before") && phone.contains(".tabbar a[aria-current=\"page\"]::after"), "curved flares join the drop to the bar edge");
}

#[test]
fn the_top_bar_is_fixed_and_the_content_clears_both_bars() {
    let css = css();
    let phone = phone(&css);
    let top = rule(phone, ".topbar {");
    assert!(top.contains("position: sticky") && top.contains("top: 0"), "{top}");
    assert!(top.contains("display: grid") && top.contains("grid-template-columns: 1fr auto 1fr"), "theme, brand, sign-out: {top}");
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
