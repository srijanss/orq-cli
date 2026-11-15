//! The phone layout, checked on the stylesheet and the page head. There is no layout engine in
//! the test setup, so these resolve the value a 360px-wide phone ends up with (top-level rules and
//! the phone breakpoint, later rules winning) for the rules that keep a narrow screen from
//! scrolling sideways and keep controls tap-sized, for widths from 320 to 430 pixels.

mod web_cascade;

use web_cascade::*;

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

/// The font size in pixels an element named by `target` ends up with at `width`, from whichever
/// `font` or `font-size` declaration wins (rem and em as 16px, `max()`/`min()` evaluated). A value
/// that cannot be shown to be a size, such as `font: inherit`, is None.
fn font_size_px(css: &str, target: &str, width: f64) -> Option<f64> {
    let element = web_cascade::subject(target);
    let mut best: Option<((bool, usize, usize), String, String)> = None;
    let mut order = 0;
    for rule in parse(css, None).iter().filter(|r| applies(&r.media, width)) {
        for selector in rule.selectors.iter().filter(|s| can_match(s, element)) {
            for (p, v) in rule.declarations.iter().filter(|(p, _)| p == "font" || p == "font-size") {
                order += 1;
                let key = (v.contains("!important"), specificity(selector), order);
                if best.as_ref().map_or(true, |(k, _, _)| key > *k) {
                    best = Some((key, p.clone(), v.replace("!important", "").trim().to_string()));
                }
            }
        }
    }
    let (_, property, value) = best?;
    if property == "font-size" {
        return length_px(&value);
    }
    // The `font` shorthand: the size is the first token that is a length, before any `/line-height`.
    value.split_whitespace().find_map(|token| length_px(token.split('/').next().unwrap_or(token)))
}

fn length_px(value: &str) -> Option<f64> {
    let value = value.trim();
    for (function, pick) in [("max(", f64::max as fn(f64, f64) -> f64), ("min(", f64::min)] {
        if let Some(args) = value.strip_prefix(function).and_then(|v| v.strip_suffix(')')) {
            return args.split(',').map(length_px).collect::<Option<Vec<f64>>>()?.into_iter().reduce(pick);
        }
    }
    if let Some(n) = value.strip_suffix("rem").or_else(|| value.strip_suffix("em")) {
        return n.trim().parse::<f64>().ok().map(|n| n * 16.0);
    }
    value.strip_suffix("px")?.trim().parse().ok()
}

const FIELDS: [&str; 6] = ["input", "select", "textarea", ".search-box input", ".signin input", ".field input"];
const FIELD_WIDTHS: [f64; 4] = [320.0, 360.0, 390.0, 430.0];

/// Every field that is under 16px at some phone width, as "target at width: size".
fn fields_under_16px(css: &str) -> Vec<String> {
    let mut small = Vec::new();
    for target in FIELDS {
        for width in FIELD_WIDTHS {
            let size = font_size_px(css, target, width);
            if !size.is_some_and(|px| px >= 16.0) {
                small.push(format!("{target} at {width}px: {size:?}"));
            }
        }
    }
    small
}

#[test]
fn on_a_phone_text_fields_are_at_least_16px_so_focusing_them_does_not_zoom() {
    let css = css();
    assert_eq!(fields_under_16px(&css), Vec::<String>::new(), "iOS zooms into a focused field under 16px");
}

#[test]
fn a_later_more_specific_rule_that_shrinks_a_field_under_16px_is_caught() {
    let shrunk = css() + "\n@media (max-width: 700px) { .search-box input:not([type=checkbox]):not([type=radio]) { font-size: 0.875rem; } }";
    let small = fields_under_16px(&shrunk);
    // Only the inputs it can reach are reported; select and textarea keep their phone size.
    let reached: Vec<&str> = FIELDS.into_iter().filter(|f| small.iter().any(|s| s.starts_with(&format!("{f} at ")))).collect();
    assert_eq!(reached, ["input", ".search-box input", ".signin input", ".field input"], "{small:?}");
    assert!(small.iter().all(|s| s.ends_with("Some(14.0)")), "{small:?}");
}
