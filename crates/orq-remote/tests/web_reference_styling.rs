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
