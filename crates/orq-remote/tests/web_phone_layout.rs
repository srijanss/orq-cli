//! The phone layout, checked on the stylesheet and the page head. There is no layout engine in
//! the test setup, so these resolve the value a 360px-wide phone ends up with (top-level rules and
//! the phone breakpoint, later rules winning) for the rules that keep a narrow screen from
//! scrolling sideways and keep controls tap-sized, for widths from 320 to 430 pixels.

use std::path::Path;

const PHONE: &str = "(max-width: 700px)";

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap().to_lowercase()
}

fn css() -> String {
    read("web/app.css")
}

struct Rule {
    media: Option<String>,
    selectors: Vec<String>,
    declarations: Vec<(String, String)>,
}

/// Every rule in source order with the @media condition it sits in (None at the top level).
fn parse(css: &str, media: Option<&str>) -> Vec<Rule> {
    let mut rules = Vec::new();
    let mut at = 0;
    while let Some(open) = css[at..].find('{').map(|i| i + at) {
        let head = css[at..open].trim();
        let mut depth = 1;
        let mut close = open + 1;
        for (i, c) in css[open + 1..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                close = open + 1 + i;
                break;
            }
        }
        let inner = &css[open + 1..close];
        if let Some(condition) = head.strip_prefix("@media") {
            rules.extend(parse(inner, Some(condition.trim())));
        } else {
            rules.push(Rule {
                media: media.map(str::to_string),
                selectors: head.split(',').map(|s| s.trim().to_string()).collect(),
                declarations: inner.split(';').filter_map(|d| d.split_once(':')).map(|(p, v)| (p.trim().to_string(), v.trim().to_string())).collect(),
            });
        }
        at = close + 1;
    }
    rules
}

/// What a phone ends up with for `property` on `selector`: top-level and phone-breakpoint rules
/// apply, the later one wins, and rules in other conditions (the colour scheme) are skipped.
fn effective(css: &str, selector: &str, property: &str) -> String {
    parse(css, None)
        .iter()
        .filter(|r| r.media.as_deref().map_or(true, |m| m == PHONE) && r.selectors.iter().any(|s| s == selector))
        .flat_map(|r| r.declarations.iter().filter(|(p, _)| p == property))
        .last()
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("no {property} for {selector} on a phone"))
}

#[test]
fn a_later_or_desktop_only_rule_changes_what_the_phone_gets() {
    let sheet = ".a { white-space: pre-wrap; }\n.a { white-space: pre; }\n@media (min-width: 701px) { .b { white-space: pre-wrap; } }\n@media (max-width: 700px) { .c { padding: 1px; } }";
    assert_eq!(effective(sheet, ".a", "white-space"), "pre");
    assert_eq!(effective(sheet, ".c", "padding"), "1px");
    assert!(std::panic::catch_unwind(|| effective(".b { x: y; }\n@media (min-width: 701px) { .b { white-space: pre-wrap; } }", ".b", "white-space")).is_err(), "a desktop-only rule is not a phone rule");
}

#[test]
fn the_page_scales_to_the_device_width_and_never_blocks_zoom() {
    let html = read("web/index.html");
    assert!(html.contains("name=\"viewport\"") && html.contains("width=device-width") && html.contains("initial-scale=1"), "{html}");
    assert!(!html.contains("user-scalable=no") && !html.contains("maximum-scale"), "zooming stays possible");
}

#[test]
fn log_lines_wrap_instead_of_running_off_the_screen() {
    let css = css();
    assert_eq!(effective(&css, ".log-lines", "white-space"), "pre-wrap");
    assert_eq!(effective(&css, ".log-lines", "overflow-wrap"), "anywhere");
    assert_eq!(effective(&css, ".log-lines", "max-width"), "100%");
}

#[test]
fn no_rule_fixes_a_width_wider_than_the_smallest_phone() {
    // 320 css pixels less the content padding on both sides.
    let css = css();
    for rule in parse(&css, None).iter().filter(|r| r.media.as_deref().map_or(true, |m| m == PHONE)) {
        for (property, value) in &rule.declarations {
            if property == "width" || property == "min-width" {
                if let Some(px) = value.strip_suffix("px") {
                    let px: f64 = px.trim().parse().unwrap();
                    assert!(px <= 288.0, "{:?} {property}: {value} is wider than a 320px screen leaves", rule.selectors);
                }
            }
        }
    }
    assert!(effective(&css, ".projects", "grid-template-columns").contains("minmax(min(16rem, 100%)"), "a project card never forces a column wider than the screen");
}

#[test]
fn rows_headers_and_log_tools_wrap_onto_more_lines() {
    let css = css();
    for selector in [".page-header", ".log-tools", ".event"] {
        assert_eq!(effective(&css, selector, "flex-wrap"), "wrap", "{selector} wraps");
    }
    assert_eq!(effective(&css, ".feature", "overflow-wrap"), "anywhere");
}

#[test]
fn every_control_is_at_least_44_pixels_tall() {
    let css = css();
    for selector in ["button", "input", "select", "a"] {
        assert_eq!(effective(&css, selector, "min-height"), "44px", "{selector}");
    }
}

#[test]
fn on_a_phone_the_content_is_tighter_and_never_scrolls_sideways() {
    let css = css();
    assert_eq!(effective(&css, ".content", "padding"), "0.75rem");
    assert_eq!(effective(&css, "body", "overflow-x"), "hidden");
    // The phone value comes from the breakpoint, not only the desktop rule.
    assert!(parse(&css, None).iter().any(|r| r.media.as_deref() == Some(PHONE) && r.selectors.iter().any(|s| s == ".content")));
}
