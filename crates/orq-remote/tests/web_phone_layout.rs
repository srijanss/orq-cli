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
fn installed_full_screen_the_bars_and_content_stay_clear_of_the_notch_and_home_indicator() {
    let html = read("web/index.html");
    assert!(html.contains("viewport-fit=cover"), "the page draws under the notch, so it must pad itself: {html}");
    let css = css();
    assert!(effective(&css, ".topbar", "padding-top").contains("env(safe-area-inset-top)"), "the top bar clears the status bar");
    assert!(effective(&css, ".tabbar", "padding-bottom").contains("env(safe-area-inset-bottom)"), "the tab bar clears the home indicator");
    assert!(effective(&css, ".content", "padding-left").contains("env(safe-area-inset-left)"), "content clears a landscape notch on the left");
    assert!(effective(&css, ".content", "padding-right").contains("env(safe-area-inset-right)"), "content clears a landscape notch on the right");
}

#[test]
fn on_a_landscape_phone_or_tablet_the_sidebar_and_content_stay_clear_of_the_notch_and_status_bar() {
    // viewport-fit=cover applies at every width, and a notched phone turned sideways or an iPad is wider
    // than the phone breakpoint, so the sidebar layout pads by the insets too.
    let css = css();
    for width in [740.0, 932.0, 1024.0, 1366.0] {
        for (selector, sides) in [(".sidebar", ["top", "left", "bottom"]), (".content", ["top", "right", "bottom"])] {
            for side in sides {
                let value = effective_at(&css, selector, &format!("padding-{side}"), width);
                assert!(value.contains(&format!("env(safe-area-inset-{side})")), "{selector} padding-{side} at {width}px: {value}");
            }
        }
    }
    // On a phone the sticky top bar already clears the status bar, so the content under it does not pad twice.
    for width in WIDTHS {
        for rule in parse(&css, None).iter().filter(|r| applies(&r.media, width) && r.selectors.iter().any(|s| can_match(s, ".content"))) {
            for (property, value) in rule.declarations.iter().filter(|(p, _)| p == "padding" || p == "padding-top") {
                assert!(!value.contains("safe-area-inset-top"), "{width}px {property}: {value}");
            }
        }
    }
}

#[test]
fn pulling_past_the_top_does_not_bounce_or_reload_the_installed_app() {
    let css = css();
    assert_eq!(effective(&css, "html", "overscroll-behavior-y"), "none");
    assert_eq!(effective(&css, "body", "overscroll-behavior-y"), "none");
}

#[test]
fn a_tap_shows_no_grey_flash_but_keyboard_focus_still_shows_a_ring() {
    let css = css();
    for selector in ["button", "a"] {
        assert_eq!(effective(&css, selector, "-webkit-tap-highlight-color"), "transparent", "{selector}");
    }
    assert!(css.contains(":focus-visible"), "focus rings stay");
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
    let mut best: Option<((bool, usize, usize), String, String)> = None;
    let mut order = 0;
    for rule in parse(css, None).iter().filter(|r| applies(&r.media, width)) {
        for selector in rule.selectors.iter().filter(|s| matches_element(s, target)) {
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
    // Only the field it reaches is reported; other inputs, select and textarea keep their phone size.
    let reached: Vec<&str> = FIELDS.into_iter().filter(|f| small.iter().any(|s| s.starts_with(&format!("{f} at ")))).collect();
    assert_eq!(reached, [".search-box input"], "{small:?}");
    assert!(small.iter().all(|s| s.ends_with("Some(14.0)")), "{small:?}");
}

#[test]
fn a_16px_rule_for_one_field_does_not_hide_a_smaller_one_on_another() {
    let fields = "input:not([type=checkbox]):not([type=radio]), select, textarea { font-size: 16px; }\n\
        @media (max-width: 700px) {\n\
          .search-box input:not([type=checkbox]):not([type=radio]) { font-size: 16px !important; }\n\
          .signin input:not([type=checkbox]):not([type=radio]) { font-size: 0.875rem; }\n\
        }";
    let small = fields_under_16px(fields);
    assert!(small.iter().any(|s| s.starts_with(".signin input at ")), "{small:?}");
    assert!(small.iter().all(|s| s.starts_with(".signin input at ")), "{small:?}");
}

#[test]
fn a_rule_under_one_ancestor_does_not_reach_a_field_under_another() {
    let sheet = ".search-box input { font-size: 10px; }\n.signin input { font-size: 18px; }\n.content .signin input { font-size: 20px; }";
    assert_eq!(font_size_px(sheet, ".signin input", 360.0), Some(18.0));
    assert_eq!(font_size_px(sheet, ".content .signin input", 360.0), Some(20.0));
    assert_eq!(font_size_px(sheet, ".search-box input", 360.0), Some(10.0));
    // A bare input sits under neither, and an ancestor named only by its tag is assumed to match.
    assert_eq!(font_size_px(sheet, "input", 360.0), None);
    assert_eq!(font_size_px("body input { font-size: 17px; }", "input", 360.0), Some(17.0));
    // A field is no particular class itself, so a rule naming one does not reach it.
    assert_eq!(font_size_px("input.tiny { font-size: 10px; }", "input", 360.0), None);
}

#[test]
fn a_sibling_rule_that_could_reach_a_field_fails_rather_than_counting_as_an_ancestor() {
    let resolve = |sheet: &'static str| std::panic::catch_unwind(move || font_size_px(sheet, ".signin input", 360.0));
    // `.signin + input` is not an input inside .signin, so its 16px must not hide the 14px one.
    assert!(resolve(".signin + input { font-size: 16px !important; }\n.signin input { font-size: 14px; }").is_err());
    assert!(resolve(".signin ~ input { font-size: 16px; }").is_err());
    // A sibling rule for another element is not in the way.
    assert_eq!(resolve(".signin + select { font-size: 10px; }\n.signin input { font-size: 17px; }").unwrap(), Some(17.0));
}

#[test]
fn an_element_match_splits_only_on_combinators_outside_attributes_and_refuses_siblings() {
    assert!(matches_element(".signin > input", ".signin input"));
    assert!(!matches_element(".signin + select", ".signin input"));
    for sibling in [".signin + input", ".signin ~ input", ".signin+input"] {
        assert!(std::panic::catch_unwind(|| matches_element(sibling, ".signin input")).is_err(), "{sibling}");
    }
}

#[test]
fn an_attribute_qualified_rule_that_could_reach_a_field_fails_rather_than_counting_as_a_match() {
    let resolve = |sheet: &'static str| std::panic::catch_unwind(move || font_size_px(sheet, ".signin input", 360.0));
    // Inputs without class x stay at 14px, so the 16px rule for `[class~=x]` must not hide them.
    assert!(resolve(".signin input[class~=x] { font-size: 16px !important; }\n.signin input { font-size: 14px; }").is_err());
    assert!(resolve(".signin[data-open] input { font-size: 16px; }").is_err());
    // An attribute inside :not(...) only rules out other inputs, as the shipped phone rule does.
    assert_eq!(resolve(".signin input:not([type=checkbox]):not([type=radio]) { font-size: 17px; }").unwrap(), Some(17.0));
    // `~=` inside an attribute is not a sibling combinator, and an attribute on another tag is not in the way.
    assert_eq!(resolve(".signin select[class~=x] { font-size: 10px; }\n.signin input { font-size: 17px; }").unwrap(), Some(17.0));
}

#[test]
fn an_element_match_refuses_attributes_outside_not_on_a_compound_that_reaches_the_field() {
    for unknown in [".signin input[class~=x]", ".signin[data-open] input", "[data-x] input", ".signin > input[type=text]"] {
        assert!(std::panic::catch_unwind(|| matches_element(unknown, ".signin input")).is_err(), "{unknown}");
    }
    assert!(matches_element(".signin input:not([type=checkbox]):not([type=radio])", ".signin input"));
    assert!(!matches_element(".signin select[class~=x]", ".signin input"));
}

#[test]
fn an_attribute_rule_that_cannot_reach_a_field_is_not_in_the_way() {
    // An unrelated ancestor or a class on the element rules these out before their attributes matter.
    for unrelated in [".other[data-open] input", ".signin input.special[data-x]", "#x[data-x] input", ".signin input#x[data-x]"] {
        assert!(!matches_element(unrelated, ".signin input"), "{unrelated}");
    }
    assert_eq!(font_size_px(".other[data-open] input { font-size: 10px; }\n.signin input { font-size: 17px; }", ".signin input", 360.0), Some(17.0));
}

#[test]
fn only_a_checkbox_or_radio_exclusion_counts_as_reaching_every_field() {
    // Checkboxes and radios are never phone fields, so leaving them out still reaches every field.
    for reaches in [".signin input:not([type=checkbox]):not([type=radio])", ".signin input:not([type=\"checkbox\"]):not([type='radio'])"] {
        assert!(matches_element(reaches, ".signin input"), "{reaches}");
    }
    // Any other negated attribute could leave out the field itself, on it or on an ancestor, however nested.
    for unknown in [
        ".signin input:not([type=text])",
        ".signin:not([data-open]) input",
        ".signin input:not(:not([type=checkbox]))",
        ".signin input:not([type=checkbox], [data-x])",
        ".signin input:not(.x, [data-x])",
    ] {
        assert!(std::panic::catch_unwind(|| matches_element(unknown, ".signin input")).is_err(), "{unknown}");
    }
    let hides = ".signin input:not([type=text]) { font-size: 16px !important; }\n.signin input { font-size: 14px; }";
    assert!(std::panic::catch_unwind(|| font_size_px(hides, ".signin input", 360.0)).is_err());
}

#[test]
fn a_dot_or_hash_inside_an_attribute_value_is_not_a_class_or_id() {
    // These reach `.signin input` for some attribute value, so they fail rather than drop out.
    for unknown in [
        ".signin[data-theme=\"a.b\"] input",
        ".signin[data-target=\"#panel\"] input",
        ".signin input[data-x=\"a.b\"]",
        ".signin input[data-x='#p']",
        ".signin input[data-x=\"a:b.c\"]",
    ] {
        assert!(std::panic::catch_unwind(|| matches_element(unknown, ".signin input")).is_err(), "{unknown}");
    }
    // A real class beside the attribute still rules the selector out.
    assert!(!matches_element(".other[data-theme=\"a.signin\"] input", ".signin input"));
}

#[test]
fn a_bracket_inside_a_quoted_attribute_value_does_not_end_the_attribute() {
    // The quoted `]` is part of the value, so what follows it is still inside the attribute.
    for unknown in [
        ".signin input[data-x=\"].b\"]",
        ".signin input[data-x='].b']",
        ".signin[data-x=\"]#p\"] input",
        ".signin input[data-x=\"] .b\"]",
        ".signin input[data-x=\"\\\"].b\"]",
    ] {
        assert!(std::panic::catch_unwind(|| matches_element(unknown, ".signin input")).is_err(), "{unknown}");
    }
}
