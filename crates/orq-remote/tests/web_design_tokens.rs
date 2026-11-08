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
        let rule = block(&css, &format!(".badge-{token}"));
        assert!(rule.contains(&format!("var(--{token})")) && rule.contains(&format!("var(--{token}-bg)")), "{token}: rule must use its tokens: {rule}");
    }
    assert!(source.contains("{icon}") && source.contains("{label}"), "a badge renders both its icon and its text");
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
        assert!(html.contains(&format!("class=\"badge badge-{token}\"")), "{token}: {html}");
        assert!(html.contains("<span aria-hidden=\"true\">") && !html.contains("<span aria-hidden=\"true\"></span>"), "{token}: {html}");
        let words = html.split("</span><span>").nth(1).unwrap_or_else(|| panic!("{token}: the words follow the icon: {html}"));
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
        assert!(html.contains("class=\"badge\"") && html.contains("Unknown"), "{key}: {html}");
    }
}
