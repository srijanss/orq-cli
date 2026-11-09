//! The sign-in page is a centred card on a full-height page, like the reference login: brand,
//! heading, stacked fields, full-width primary buttons and the theme switch at the bottom.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

#[test]
fn the_card_sits_centred_in_a_full_height_page() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "responses": [], "steps": [], "selectors": [".signin-page > .signin", ".signin h1", ".signin .primary"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let m = &out["frames"][0]["settled"]["matches"];
    assert_eq!(out["frames"][0]["settled"]["matches"][".signin h1"], 0, "no heading above the buttons: {out}");
    for k in [".signin-page > .signin", ".signin .primary"] {
        assert_eq!(m[k], 1, "{k}: {out}");
    }
}

#[test]
fn the_page_centres_a_wide_card_and_the_primary_button_fills_it() {
    let css = css();
    let page = rule(&css, ".signin-page {");
    assert!(page.contains("min-height: 100dvh") && page.contains("display: flex") && page.contains("align-items: center") && page.contains("justify-content: center"), "{page}");
    let card = rule(&css, "\n.signin {");
    assert!(card.contains("max-width: 28rem") && card.contains("width: 100%"), "{card}");
    let primary = rule(&css, ".signin .primary {");
    assert!(primary.contains("width: 100%") && primary.contains("background: var(--primary)") && primary.contains("color: var(--on-primary)"), "{primary}");
    assert!(!css.contains(".signin .theme-toggle { align-self: flex-end; margin-left: 0; order: -1; }"), "the theme switch moves to the bottom");
}

#[test]
fn the_passkey_button_carries_a_passkey_icon() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let main = std::fs::read_to_string(format!("{dir}/web-src/src/main.jsx")).unwrap();
    let icons = std::fs::read_to_string(format!("{dir}/web-src/src/icons.jsx")).unwrap();
    assert!(icons.contains("  passkey: \""), "the icon is drawn");
    let start = main.find("Sign in with a passkey").unwrap();
    assert!(main[start.saturating_sub(120)..start].contains("<Icon name=\"passkey\""), "the icon sits in the button before its label");
}

#[test]
fn the_primary_button_is_the_sidebar_pill_colour_in_dark_and_the_dark_colour_in_light() {
    let css = css();
    assert_eq!(css.matches("--primary: #dbeafe").count(), 2, "dark blocks: the sidebar's selected pill");
    assert_eq!(css.matches("--on-primary: #0f172a").count(), 2, "dark blocks: its dark text");
    assert_eq!(css.matches("--primary: #0f172a").count(), 2, "light blocks: the dark colour");
    assert_eq!(css.matches("--on-primary: #ffffff").count(), 2, "light blocks: white text");
}

#[test]
fn a_divider_line_with_space_around_it_separates_the_brand_from_the_buttons() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/dashboard", "responses": [], "steps": [], "selectors": [".signin > .brand + .signin-divider[role=\"separator\"]"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(out["frames"][0]["settled"]["matches"][".signin > .brand + .signin-divider[role=\"separator\"]"], 1, "{out}");
    let css = css();
    let line = rule(&css, ".signin-divider {");
    assert!(line.contains("height: 1px") && line.contains("background: var(--border)") && line.contains("margin: 0.5rem 0"), "{line}");
}
