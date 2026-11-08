//! The pages follow the reference look: dark by default, spec tokens, cards, pills, ticket-ID line.

mod web_deps;

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

fn css() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/app.css")).unwrap().to_lowercase()
}

/// The text of the first rule block that starts with `selector`.
fn block<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    let rest = &css[start..];
    &rest[..rest.find('}').unwrap()]
}

#[test]
fn dark_is_the_default_and_the_light_look_is_an_override() {
    let css = css();
    let root = block(&css, ":root {");
    for token in ["--bg: #0d1117", "--sidebar: #151b23", "--surface: #1f2937", "--text: #f3f4f6", "--accent: #60a5fa", "--border: #303a46"] {
        assert!(root.contains(token), "default tokens carry {token}: {root}");
    }
    let light = &css[css.find("@media (prefers-color-scheme: light)").expect("a light override exists")..];
    for token in ["--bg: #ffffff", "--sidebar: #f8fafc", "--surface: #f1f5f9", "--text: #111827", "--accent: #2563eb"] {
        assert!(light.contains(token), "light override carries {token}");
    }
}

#[test]
fn the_page_uses_the_tokens_and_cards_and_pills_are_rounded() {
    let css = css();
    assert!(block(&css, "body {").contains("var(--bg)") && block(&css, "body {").contains("var(--text)"));
    assert!(block(&css, ".sidebar {").contains("var(--sidebar)"));
    let card = block(&css, ".card {");
    assert!(card.contains("var(--surface)") && card.contains("border-radius") && card.contains("var(--border)"), "{card}");
    assert!(block(&css, ".badge {").contains("border-radius: 999px"));
    assert!(block(&css, ".ticket-id {").contains("monospace"));
}

#[test]
fn each_inbox_row_is_a_card_with_its_ticket_id_above_the_title() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let review = json!({"run_id": "r1", "feature": "add-log-export", "project": "p", "current": true, "status": "waiting", "risk": "low", "stale": false, "link": null});
    let scenario = json!({"responses": [{"status": 200, "json": {"reviews": [review]}}], "steps": [{"submit": "typed-token-0123456789abcdef0123456789"}]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let texts: Vec<&str> = out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    let id = texts.iter().position(|t| *t == "r1").expect("the ticket id is shown");
    let title = texts.iter().position(|t| *t == "add-log-export").unwrap();
    assert!(id < title, "{texts:?}");
    assert_eq!(out["frames"][1]["settled"]["cards"], 1, "{out}");
}

#[test]
fn before_a_token_is_given_a_sign_in_card_offers_signing_in_with_a_token() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"responses": [], "steps": []});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let start = &out["frames"][0]["settled"];
    let texts: Vec<&str> = start["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Sign in"), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("Sign in with token")), "{texts:?}");
    assert_eq!(start["cards"], 1, "the sign-in form sits in a card: {out}");
    assert_eq!(start["inputs"].as_array().unwrap().len(), 1, "{out}");
}

#[test]
fn the_top_bar_keeps_its_own_height_and_the_content_row_takes_the_rest() {
    let css = css();
    let shell = block(&css, ".shell {");
    assert!(shell.contains("grid-template-rows: auto 1fr"), "rows must not share the height equally: {shell}");
    let phone = &css[css.find("@media (max-width: 700px)").unwrap()..];
    assert!(phone.contains("grid-template-rows: auto 1fr"), "the phone layout too");
}

#[test]
fn an_explicit_theme_on_the_page_overrides_the_device_setting_both_ways() {
    let css = css();
    assert!(block(&css, ":root[data-theme=\"light\"] {").contains("--bg: #ffffff"));
    assert!(block(&css, ":root[data-theme=\"dark\"] {").contains("--bg: #0d1117"));
    let media = &css[css.find("@media (prefers-color-scheme: light)").unwrap()..];
    assert!(media[..media.find("}\n}").unwrap()].contains(":root:not([data-theme=\"dark\"])"), "the device's light setting must not beat an explicit dark choice");
}

fn harness(steps: Value) -> Value {
    let dir = env!("CARGO_MANIFEST_DIR");
    let reply = json!({"status": 200, "json": {"reviews": []}});
    let scenario = json!({"responses": [reply.clone(), reply], "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn the_sign_in_screen_and_the_signed_in_page_both_offer_a_theme_toggle_that_flips_the_theme() {
    if !web_deps::ready() {
        return;
    }
    let out = harness(json!([{"nav": "[data-theme-toggle]"}, {"nav": "[data-theme-toggle]"}, {"submit": "typed-token-0123456789abcdef0123456789"}, {"nav": "[data-theme-toggle]"}]));
    let themes: Vec<&Value> = out["frames"].as_array().unwrap().iter().map(|f| &f["settled"]["theme"]).collect();
    assert_eq!(themes, ["", "light", "dark", "dark", "light"], "{out}");
    assert_eq!(out["frames"][0]["settled"]["toggles"], 1, "{out}");
    assert_eq!(out["frames"][3]["settled"]["toggles"], 1, "one toggle after signing in, not two: {out}");
}

#[test]
fn the_sidebar_carries_the_brand_the_collapse_control_icons_and_the_theme_switch_like_the_reference() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let out = Command::new("node").arg(format!("{dir}/tests/web_shell_structure.mjs")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shape: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(shape["brand"], "Orq", "{shape}");
    assert_eq!(shape["railInSidebar"], true, "the collapse chevron sits in the sidebar header: {shape}");
    assert_eq!(shape["themeInSidebar"], true, "{shape}");
    assert_eq!(shape["linksWithIcon"], shape["links"], "every destination has an svg icon: {shape}");
    assert_eq!(shape["iconsHidden"], true, "icons are decoration only: {shape}");
}

#[test]
fn the_top_bar_is_only_for_phones() {
    let css = css();
    assert!(block(&css, ".topbar {").contains("display: none"), "no empty bar above the desktop layout");
    let phone = &css[css.find("@media (max-width: 700px)").unwrap()..];
    assert!(phone.contains(".topbar { display: flex"), "the phone keeps the bar for its menu button");
}

#[test]
fn the_inbox_has_a_page_header_with_title_subtitle_and_a_refresh_that_reloads() {
    if !web_deps::ready() {
        return;
    }
    let out = harness(json!([{"submit": "typed-token-0123456789abcdef0123456789"}, {"nav": "[data-refresh]"}]));
    let settled = &out["frames"][1]["settled"];
    let texts: Vec<&str> = settled["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Reviews") && texts.contains(&"Code reviews and agent reports"), "{texts:?}");
    assert_eq!(settled["refreshButtons"], 1, "{out}");
    assert_eq!(out["frames"][2]["settled"]["refreshButtons"], 1, "{out}");
    assert!(out["frames"][2]["settled"]["texts"].as_array().unwrap().iter().any(|t| t == "No reviews in the inbox."), "the list is loaded again: {out}");
}
