//! The phone layout, checked on the stylesheet and the page head. There is no layout engine in
//! the test setup, so these resolve the value a 360px-wide phone ends up with (top-level rules and
//! the phone breakpoint, later rules winning) for the rules that keep a narrow screen from
//! scrolling sideways and keep controls tap-sized, for widths from 320 to 430 pixels.

use std::path::Path;

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

/// Whether a media condition holds at `width` pixels. Colour-scheme and reduced-motion conditions hold no widths and
/// are skipped; any other condition this does not understand fails the test rather than hiding.
fn applies(media: &Option<String>, width: f64) -> bool {
    let Some(condition) = media else { return true };
    if condition.contains("prefers-color-scheme") || condition.contains("prefers-reduced-motion") {
        return false;
    }
    condition.split(" and ").all(|part| {
        let part = part.trim().trim_start_matches('(').trim_end_matches(')');
        let (feature, value) = part.split_once(':').unwrap_or_else(|| panic!("media condition {condition:?} is not understood"));
        let px: f64 = value.trim().strip_suffix("px").and_then(|v| v.parse().ok()).unwrap_or_else(|| panic!("media condition {condition:?} is not understood"));
        match feature.trim() {
            "max-width" => width <= px,
            "min-width" => width >= px,
            _ => panic!("media condition {condition:?} is not understood"),
        }
    })
}

/// The last compound selector (the part that names the element itself).
fn subject(selector: &str) -> &str {
    selector.rsplit([' ', '>', '+', '~']).next().unwrap_or(selector)
}

/// Whether a rule's selector can match an element that `target` (a class or a tag) names. A
/// selector that adds ancestors or other classes is assumed to match, which is the safe direction.
fn can_match(selector: &str, target: &str) -> bool {
    let compound = subject(selector);
    if let Some(class) = target.strip_prefix('.') {
        compound.split('.').skip(1).any(|c| c.split([':', '[', '#']).next() == Some(class))
    } else {
        compound.split(['.', ':', '[', '#']).next() == Some(target)
    }
}

/// (ids, classes/attributes/pseudo-classes, tags) as one comparable number.
fn specificity(selector: &str) -> usize {
    let mut score = 0;
    for compound in selector.split([' ', '>', '+', '~']).filter(|c| !c.is_empty()) {
        score += compound.matches('#').count() * 10_000 + (compound.matches('.').count() + compound.matches('[').count() + compound.matches(':').count()) * 100;
        if compound.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            score += 1;
        }
    }
    score
}

/// What an element named by `target` ends up with for `property` on a screen `width` pixels wide:
/// every rule whose media condition holds and whose selector can match, ordered by importance,
/// specificity and source order, the last one winning.
fn effective_at(css: &str, target: &str, property: &str, width: f64) -> String {
    let mut best: Option<((bool, usize, usize), String)> = None;
    let mut order = 0;
    for rule in parse(css, None).iter().filter(|r| applies(&r.media, width)) {
        for selector in rule.selectors.iter().filter(|s| can_match(s, target)) {
            for (_, v) in rule.declarations.iter().filter(|(p, _)| p == property) {
                order += 1;
                let key = (v.contains("!important"), specificity(selector), order);
                if best.as_ref().map_or(true, |(k, _)| key > *k) {
                    best = Some((key, v.replace("!important", "").trim().to_string()));
                }
            }
        }
    }
    best.unwrap_or_else(|| panic!("no {property} for {target} at {width}px")).1
}

const WIDTHS: [f64; 3] = [320.0, 360.0, 430.0];

/// The value on every phone width, which must be one value.
fn effective(css: &str, target: &str, property: &str) -> String {
    let values: Vec<String> = WIDTHS.iter().map(|w| effective_at(css, target, property, *w)).collect();
    assert!(values.iter().all(|v| *v == values[0]), "{target} {property} differs between phone widths: {values:?}");
    values[0].clone()
}

#[test]
fn the_cascade_is_resolved_by_width_specificity_importance_and_order() {
    let at = |sheet: &str, target: &str, width: f64| effective_at(sheet, target, "white-space", width);
    // A later rule for the same selector wins.
    assert_eq!(at(".a { white-space: pre-wrap; }\n.a { white-space: pre; }", ".a", 360.0), "pre");
    // A more specific selector wins even when it comes first, and a descendant selector counts.
    assert_eq!(at("body .a { white-space: pre; }\n.a { white-space: pre-wrap; }", ".a", 360.0), "pre");
    assert_eq!(at("pre.a { white-space: pre; }\n.a { white-space: pre-wrap; }", ".a", 360.0), "pre");
    // !important beats specificity.
    assert_eq!(at(".a { white-space: pre-wrap !important; }\nbody .a { white-space: pre; }", ".a", 360.0), "pre-wrap");
    // Narrower and compound conditions apply at the widths inside them and not outside.
    let narrow = ".a { white-space: pre-wrap; }\n@media (max-width: 400px) { .a { white-space: pre; } }";
    assert_eq!((at(narrow, ".a", 360.0), at(narrow, ".a", 430.0)), ("pre".into(), "pre-wrap".into()));
    let compound = ".a { white-space: pre-wrap; }\n@media (min-width: 300px) and (max-width: 340px) { .a { white-space: pre; } }";
    assert_eq!((at(compound, ".a", 320.0), at(compound, ".a", 360.0)), ("pre".into(), "pre-wrap".into()));
    // A desktop-only rule is not a phone rule.
    assert_eq!(at(".a { white-space: pre-wrap; }\n@media (min-width: 701px) { .a { white-space: pre; } }", ".a", 360.0), "pre-wrap");
    // A value that differs between phone widths is reported rather than averaged away.
    assert!(std::panic::catch_unwind(|| effective(narrow, ".a", "white-space")).is_err());
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
    for rule in parse(&css, None).iter().filter(|r| WIDTHS.iter().any(|w| applies(&r.media, *w))) {
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
    // The phone value comes from the breakpoint: a wide screen keeps the roomier padding.
    assert_ne!(effective_at(&css, ".content", "padding", 1200.0), "0.75rem");
}
