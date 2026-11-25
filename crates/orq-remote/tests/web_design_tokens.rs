//! Semantic colour tokens for light and dark, and badges that never rely on colour alone.

mod web_deps;

use std::path::{Path, PathBuf};

const TOKENS: [&str; 10] = [
    "status-running", "status-completed", "status-failed", "status-waiting", "status-queued",
    "risk-low", "risk-medium", "risk-high", "freshness-fresh", "freshness-outdated",
];

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn css() -> String {
    std::fs::read_to_string(crate_dir().join("web/app.css")).unwrap()
}

/// The text between the braces of the first block that starts with `head`.
fn block<'a>(text: &'a str, head: &str) -> &'a str {
    let start = text.find(head).unwrap_or_else(|| panic!("no `{head}` block"));
    let open = start + text[start..].find('{').unwrap() + 1;
    let mut depth = 1;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unclosed `{head}` block");
}

#[test]
fn every_token_has_a_light_and_a_dark_value() {
    let css = css();
    let dark = block(&css, ":root");
    let light_media = block(&css, "@media (prefers-color-scheme: light)");
    let light = block(light_media, ":root");
    for token in TOKENS {
        for suffix in ["", "-bg"] {
            let name = format!("--{token}{suffix}:");
            assert!(light.contains(&name), "light value missing for {name}");
            assert!(dark.contains(&name), "dark value missing for {name}");
        }
    }
}

#[test]
fn every_token_has_a_badge_with_a_label_and_an_icon_that_uses_it() {
    let css = css();
    let source = std::fs::read_to_string(crate_dir().join("web-src/src/badge.jsx")).unwrap();
    for token in TOKENS {
        let entry = format!("\"{token}\": {{ label: \"");
        let at = source.find(&entry).unwrap_or_else(|| panic!("badge.jsx has no entry for {token}"));
        let rest = &source[at + entry.len()..];
        let label = &rest[..rest.find('"').unwrap()];
        assert!(!label.trim().is_empty(), "{token}: empty label");
        let icon_at = rest.find("icon: \"").unwrap_or_else(|| panic!("{token}: no icon")) + 7;
        let icon = &rest[icon_at..icon_at + rest[icon_at..].find('"').unwrap()];
        assert!(!icon.trim().is_empty(), "{token}: empty icon");
        let rule = block(&css, &format!(".status-pill.tone-{token} "));
        assert!(rule.contains(&format!("var(--{token})")) && rule.contains(&format!("var(--{token}-bg)")), "{token}: rule must use its tokens: {rule}");
    }
    assert!(source.contains("<Icon name={glyph}") && source.contains("{words}"), "a pill renders both its icon and its words");
}

/// Renders the given kinds with the real component; None when node or the web dependencies are missing.
fn render(kinds: &[serde_json::Value]) -> Option<serde_json::Value> {
    web_deps::ready().then(|| {
        let output = std::process::Command::new("node")
            .arg(crate_dir().join("tests/web_badge_render.mjs"))
            .arg(serde_json::Value::Array(kinds.to_vec()).to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

#[test]
fn each_badge_renders_its_icon_hidden_from_readers_its_text_and_its_class() {
    let kinds: Vec<serde_json::Value> = TOKENS.iter().map(|t| serde_json::json!(t)).collect();
    let Some(out) = render(&kinds) else { return };
    for token in TOKENS {
        let entry = &out["html"][token];
        assert_eq!(entry["ok"], true, "{token}: {entry}");
        let html = entry["html"].as_str().unwrap();
        assert!(html.contains(&format!("class=\"status-pill tone-{token}\"")), "{token}: {html}");
        assert!(html.starts_with("<span") && html.contains("><svg class=\"icon\"") && html.contains("aria-hidden=\"true\""), "{token}: the icon comes first, hidden from readers: {html}");
        let words = html.split("</svg><span>").nth(1).unwrap_or_else(|| panic!("{token}: the words follow the icon: {html}"));
        assert!(words.starts_with(|c: char| c.is_alphabetic()), "{token}: the words follow the icon: {html}");
    }
}

#[test]
fn an_unknown_or_omitted_kind_renders_a_plain_badge_instead_of_throwing() {
    let kinds = [serde_json::json!("nonsense"), serde_json::json!(null), serde_json::json!("constructor")];
    let Some(out) = render(&kinds) else { return };
    for key in ["nonsense", "null", "constructor"] {
        let entry = &out["html"][key];
        assert_eq!(entry["ok"], true, "{key}: {entry}");
        let html = entry["html"].as_str().unwrap();
        assert!(html.contains("class=\"status-pill tone-neutral\"") && html.contains("Unknown"), "{key}: {html}");
    }
}

/// The words between the pill's icon and its end.
fn pill_words(html: &str) -> &str {
    let words = html.split("</svg><span>").nth(1).unwrap_or_else(|| panic!("the words follow the icon: {html}"));
    &words[..words.find("</span>").unwrap()]
}

#[test]
fn every_status_pill_kind_says_words_after_an_icon_never_its_enum_name() {
    let Some(out) = render(&[]) else { return };
    let kinds = out["pillKinds"].as_array().unwrap();
    assert!(kinds.len() >= 20, "{out}");
    for kind in kinds.iter().map(|k| k.as_str().unwrap()) {
        let entry = &out["pills"][kind];
        assert_eq!(entry["ok"], true, "{kind}: {entry}");
        let html = entry["html"].as_str().unwrap();
        assert!(html.starts_with("<span class=\"status-pill tone-"), "{kind}: {html}");
        let words = pill_words(html);
        assert!(!words.contains('_') && words.starts_with(|c: char| c.is_uppercase()), "{kind}: `{words}` is words, not an enum name");
    }
    assert_eq!(pill_words(out["pills"]["in_progress"]["html"].as_str().unwrap()), "In progress");
}

#[test]
fn a_status_pill_kind_it_does_not_know_is_neutral_and_says_the_word_in_sentence_case() {
    let kinds = [serde_json::json!("in_review"), serde_json::json!(null), serde_json::json!("constructor")];
    let Some(out) = render(&kinds) else { return };
    for (key, words) in [("in_review", "In review"), ("null", "Unknown"), ("constructor", "Constructor")] {
        let entry = &out["pills"][key];
        assert_eq!(entry["ok"], true, "{key}: {entry}");
        let html = entry["html"].as_str().unwrap();
        assert!(html.contains("class=\"status-pill tone-neutral\""), "{key}: {html}");
        assert_eq!(pill_words(html), words, "{key}: {html}");
    }
}
