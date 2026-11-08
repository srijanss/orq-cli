//! The phone layout, checked on the stylesheet and the page head. There is no layout engine in
//! the test setup, so these check the rules that keep a narrow screen from scrolling sideways
//! and keep controls tap-sized, for widths from 320 to 430 pixels.

use std::path::Path;

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap().to_lowercase()
}

fn css() -> String {
    read("web/app.css")
}

/// The declarations of every rule whose selector list includes `selector` exactly, joined.
fn rules(css: &str, selector: &str) -> String {
    let mut found = String::new();
    for chunk in css.split('}') {
        if let Some((selectors, body)) = chunk.rsplit_once('{') {
            let selectors = selectors.rsplit(['}', ';']).next().unwrap_or(selectors);
            if selectors.split(',').any(|s| s.trim() == selector) {
                found.push_str(body);
                found.push(';');
            }
        }
    }
    assert!(!found.is_empty(), "no rule for {selector}");
    found
}

#[test]
fn the_page_scales_to_the_device_width_and_never_blocks_zoom() {
    let html = read("web/index.html");
    assert!(html.contains("name=\"viewport\"") && html.contains("width=device-width") && html.contains("initial-scale=1"), "{html}");
    assert!(!html.contains("user-scalable=no") && !html.contains("maximum-scale"), "zooming stays possible");
}

#[test]
fn log_lines_wrap_instead_of_running_off_the_screen() {
    let log = rules(&css(), ".log-lines");
    assert!(log.contains("white-space: pre-wrap"), "{log}");
    assert!(log.contains("overflow-wrap: anywhere") || log.contains("word-break: break-all"), "{log}");
    assert!(log.contains("max-width: 100%"), "{log}");
}

#[test]
fn no_rule_fixes_a_width_wider_than_the_smallest_phone() {
    // 320 css pixels less the content padding on both sides.
    let css = css();
    for declaration in css.split(';') {
        let declaration = declaration.rsplit(['{', '}']).next().unwrap().trim();
        if let Some(value) = declaration.strip_prefix("width:").or_else(|| declaration.strip_prefix("min-width:")) {
            if let Some(px) = value.trim().strip_suffix("px") {
                let px: f64 = px.trim().parse().unwrap();
                assert!(px <= 288.0, "{declaration} is wider than a 320px screen leaves");
            }
        }
    }
    let grid = rules(&css, ".projects");
    assert!(grid.contains("minmax(min(16rem, 100%)"), "a project card never forces a column wider than the screen: {grid}");
}

#[test]
fn rows_headers_and_log_tools_wrap_onto_more_lines() {
    let css = css();
    for selector in [".page-header", ".log-tools", ".event"] {
        assert!(rules(&css, selector).contains("flex-wrap: wrap"), "{selector} wraps");
    }
}

#[test]
fn feature_text_breaks_long_words() {
    assert!(rules(&css(), ".feature").contains("overflow-wrap: anywhere"));
}

#[test]
fn every_control_is_at_least_44_pixels_tall() {
    let css = css();
    for selector in ["button", "input", "select", "a"] {
        let rule = rules(&css, selector);
        assert!(rule.contains("min-height: 44px"), "{selector}: {rule}");
    }
}

#[test]
fn on_a_phone_the_content_is_tighter_and_never_scrolls_sideways() {
    let css = css();
    let phone = &css[css.find("@media (max-width: 700px)").unwrap()..];
    let phone = &phone[..phone.find("\n}").unwrap()];
    assert!(phone.contains(".content") && phone.contains("padding: 0.75rem"), "{phone}");
    let body = rules(&css, "body");
    assert!(body.contains("overflow-x: hidden") || rules(&css, ".content").contains("overflow-x"), "{body}");
}
