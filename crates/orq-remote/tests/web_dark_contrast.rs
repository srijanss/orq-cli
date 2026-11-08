//! The dark theme uses the spec's tokens, and in both themes every text colour stays readable
//! (4.5:1) on the surface it is drawn on. Colours live in tokens, never as literals in rules.

use std::collections::HashMap;
use std::path::Path;

fn css() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/app.css")).unwrap()
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

fn tokens(block: &str) -> HashMap<String, String> {
    block.lines().filter_map(|l| l.trim().strip_prefix("--")).filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().trim_end_matches(';').trim().to_string())).collect()
}

fn channel(hex: &str, at: usize) -> f64 {
    let v = u8::from_str_radix(&hex[at..at + 2], 16).unwrap() as f64 / 255.0;
    if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn luminance(colour: &str) -> f64 {
    let hex = colour.strip_prefix('#').unwrap_or_else(|| panic!("`{colour}` is not a #rrggbb colour"));
    assert_eq!(hex.len(), 6, "{colour}");
    0.2126 * channel(hex, 0) + 0.7152 * channel(hex, 2) + 0.0722 * channel(hex, 4)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn themes() -> [(&'static str, HashMap<String, String>); 2] {
    let css = css();
    let light_media = block(&css, "@media (prefers-color-scheme: light)");
    [("dark", tokens(block(&css, ":root {"))), ("light", tokens(block(light_media, ":root")))]
}

#[test]
fn the_dark_theme_uses_the_specs_tokens_whether_chosen_or_followed() {
    let css = css();
    let dark = tokens(block(&css, ":root {"));
    for (name, value) in [("bg", "#0d1117"), ("sidebar", "#151b23"), ("surface", "#1f2937"), ("text", "#f3f4f6"), ("accent", "#60a5fa")] {
        assert_eq!(dark.get(name).map(String::as_str), Some(value), "--{name}");
    }
    assert_eq!(tokens(block(&css, ":root[data-theme=\"dark\"]")), dark, "an explicit dark choice and the default must agree");
    assert_eq!(tokens(block(&css, ":root[data-theme=\"light\"]")), themes()[1].1, "an explicit light choice and the device's light setting must agree");
}

#[test]
fn text_stays_readable_on_every_surface_it_is_drawn_on_in_both_themes() {
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    for (theme, t) in themes() {
        let get = |k: &str| t.get(k).unwrap_or_else(|| panic!("{theme}: --{k} missing")).clone();
        for fg in ["text", "text-muted", "accent"] {
            for bg in ["bg", "sidebar", "surface"] {
                pairs.push((theme.into(), format!("--{fg} on --{bg}"), format!("{} {}", get(fg), get(bg))));
            }
        }
        for token in ["status-running", "status-completed", "status-failed", "status-waiting", "status-queued", "risk-low", "risk-medium", "risk-high", "freshness-fresh", "freshness-outdated"] {
            pairs.push((theme.into(), format!("badge {token}"), format!("{} {}", get(token), get(&format!("{token}-bg")))));
        }
        // The stale marker is text drawn straight on the card and on the page.
        for bg in ["bg", "surface"] {
            pairs.push((theme.into(), format!("--stale on --{bg}"), format!("{} {}", get("stale"), get(bg))));
        }
    }
    let failing: Vec<String> = pairs.iter().filter_map(|(theme, what, colours)| {
        let (a, b) = colours.split_once(' ').unwrap();
        let ratio = contrast(a, b);
        (ratio < 4.5).then(|| format!("{theme}: {what} is {ratio:.2}:1 ({a} on {b})"))
    }).collect();
    assert!(failing.is_empty(), "{failing:#?}");
}

#[test]
fn no_rule_carries_a_colour_literal_so_both_themes_reach_every_element() {
    let css = css();
    let light_media = block(&css, "@media (prefers-color-scheme: light)");
    let mut rest = css.clone();
    for head in [":root[data-theme=\"dark\"]", ":root[data-theme=\"light\"]", "@media (prefers-color-scheme: light)"] {
        let inner = block(&rest, head).to_string();
        rest = rest.replacen(&inner, "", 1);
    }
    let rest = rest.replacen(block(&rest, ":root {"), "", 1);
    let _ = light_media;
    let literals: Vec<&str> = rest.lines().filter(|l| l.contains('#') && l.contains(':') && !l.trim_start().starts_with("--")).filter(|l| l.split(|c: char| !c.is_ascii_hexdigit() && c != '#').any(|w| w.starts_with('#') && w.len() >= 4)).collect();
    assert!(literals.is_empty(), "colours must come from tokens: {literals:#?}");
}

/// The token a declaration of `property` in the last rule starting with `head` names.
fn rule_token(css: &str, head: &str, property: &str) -> String {
    let body = css.lines().filter(|l| l.starts_with(head)).map(|l| l.to_string()).collect::<Vec<_>>().join(" ");
    let after = body.split(&format!("{property}: var(--")).nth(1).unwrap_or_else(|| panic!("`{head}` has no {property} token"));
    after.split(')').next().unwrap().to_string()
}

#[test]
fn the_active_navigation_link_is_readable_on_its_own_background_in_both_themes() {
    let css = css();
    let head = ".sidebar nav a[aria-current=\"page\"]";
    let (fg, bg) = (rule_token(&css, head, "color"), rule_token(&css, head, "background"));
    for (theme, t) in themes() {
        let ratio = contrast(&t[&fg], &t[&bg]);
        assert!(ratio >= 4.5, "{theme}: active link --{fg} on --{bg} is {ratio:.2}:1");
    }
}
