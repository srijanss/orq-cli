//! On a phone the page is framed like a mobile app: a fixed top bar with the brand and a fixed
//! bottom tab bar holding the destinations. The drawer keeps only the project, theme and sign-out.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

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

#[test]
fn the_tab_bar_holds_the_five_destinations_and_marks_the_current_one() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let selectors = ["nav.tabbar a", "nav.tabbar a[aria-current=\"page\"]", "nav.tabbar a svg", "nav.tabbar a .tab-label"];
    let scenario = json!({"url": "/dashboard", "responses": [{"status": 200, "json": {"projects": []}}], "steps": [{"submit": TOKEN}], "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let m = &out["frames"][1]["settled"]["matches"];
    assert_eq!(m["nav.tabbar a"], 5, "{out}");
    assert_eq!(m["nav.tabbar a[aria-current=\"page\"]"], 1, "{out}");
    assert_eq!(m["nav.tabbar a svg"], 5, "{out}");
    assert_eq!(m["nav.tabbar a .tab-label"], 5, "{out}");
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
    let tab = rule(phone, ".tabbar a {");
    assert!(tab.contains("flex: 1") && tab.contains("flex-direction: column") && tab.contains("min-height: 56px"), "{tab}");
    assert!(rule(phone, ".tabbar a[aria-current=\"page\"] {").contains("var(--accent)"), "the current tab is accent coloured");
}

#[test]
fn the_top_bar_is_fixed_and_the_content_clears_both_bars() {
    let css = css();
    let phone = phone(&css);
    let top = rule(phone, ".topbar {");
    assert!(top.contains("position: sticky") && top.contains("top: 0"), "{top}");
    assert!(top.contains("display: grid") && top.contains("grid-template-columns: 2.75rem 1fr 2.75rem"), "menu, brand, balancing space: {top}");
    assert!(rule(phone, ".topbar .brand {").contains("justify-self: center"), "the app icon and name sit in the centre of the header");
    assert!(rule(phone, ".content {").contains("padding-bottom: calc(5rem + env(safe-area-inset-bottom))"), "content is not hidden under the tab bar");
}

#[test]
fn the_drawer_no_longer_repeats_the_destinations() {
    let css = css();
    let phone = phone(&css);
    assert!(rule(phone, ".sidebar nav > .sidebar-divider, .sidebar nav > a {").contains("display: none"), "the sidebar's links are replaced by the tab bar");
}
