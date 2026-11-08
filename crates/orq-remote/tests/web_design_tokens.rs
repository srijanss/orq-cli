//! Semantic colour tokens for light and dark, and badges that never rely on colour alone.

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
    let light = block(&css, ":root");
    let dark_media = block(&css, "@media (prefers-color-scheme: dark)");
    let dark = block(dark_media, ":root");
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
