//! A feature's description is markdown. Feature detail's Description tab draws it as formatted text,
//! built into React elements by a small parser, so nothing in the text is ever interpreted as HTML.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

fn ledger(description: Value) -> Value {
    json!({"status": 200, "json": {"features": [{"name": "next-one", "description": description, "status": "pending", "depends_on": [], "recorded_at": null}]}})
}

fn no_runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

/// The feature's detail page in jsdom, counting `selectors` once the page has settled.
fn detail(description: &str, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/features/next-one", "responses": [ledger(json!(description)), no_runs(), projects()], "steps": [{"submit": TOKEN}], "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settled(out: &Value) -> &Value {
    &out["frames"].as_array().unwrap().last().unwrap()["settled"]
}

fn count(out: &Value, selector: &str) -> u64 {
    settled(out)["matches"][selector].as_u64().unwrap()
}

fn texts(out: &Value) -> Vec<String> {
    settled(out)["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn html_shows_as_text_and_only_http_links_become_links() {
    let text = "<img src=x onerror=alert(1)> and [docs](https://example.com/a?b=1) and [plain](http://example.com) and [bad](javascript:alert(1)) and [data](data:text/html,x)";
    let selectors = [".markdown img", ".markdown script", ".markdown a", ".markdown a[target=\"_blank\"][rel=\"noopener noreferrer\"]"];
    let Some(out) = detail(text, &selectors) else { return };
    assert_eq!(count(&out, selectors[0]), 0, "no element is made from the HTML: {out}");
    assert_eq!(count(&out, selectors[1]), 0, "{out}");
    assert_eq!(count(&out, selectors[2]), 2, "only the http and https links: {out}");
    assert_eq!(count(&out, selectors[3]), 2, "each opens in a new tab without an opener: {out}");
    let links = settled(&out)["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["text"] == "docs" && l["href"] == "https://example.com/a?b=1"), "{links:?}");
    assert!(!links.iter().any(|l| l["href"].as_str().unwrap().starts_with("javascript") || l["href"].as_str().unwrap().starts_with("data")), "{links:?}");
    let shown = texts(&out).join("");
    assert!(shown.contains("<img src=x onerror=alert(1)>"), "the HTML is shown as text: {shown}");
    assert!(shown.contains("[bad](javascript:alert(1))") && shown.contains("[data](data:text/html,x)"), "a link that is not http stays as written: {shown}");
}

#[test]
fn inline_code_bold_and_italic_and_a_fenced_block_render_as_elements() {
    let text = "Use `x` and **bold** and *soft* here.\n\n```rust\nlet a = 1; // # not a heading\n\n- not a list\n```\n\nafter";
    let selectors = [".markdown p code", ".markdown p strong", ".markdown p em", ".markdown pre > code", ".markdown pre h3, .markdown pre li", ".markdown > p"];
    let Some(out) = detail(text, &selectors) else { return };
    assert_eq!(count(&out, selectors[0]), 1, "{out}");
    assert_eq!(count(&out, selectors[1]), 1, "{out}");
    assert_eq!(count(&out, selectors[2]), 1, "{out}");
    assert_eq!(count(&out, selectors[3]), 1, "{out}");
    assert_eq!(count(&out, selectors[4]), 0, "nothing inside a fence is parsed: {out}");
    assert_eq!(count(&out, selectors[5]), 2, "the paragraph before and the one after: {out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == "x") && shown.iter().any(|t| t == "bold") && shown.iter().any(|t| t == "soft"), "{shown:?}");
    assert!(shown.iter().any(|t| t.contains("let a = 1; // # not a heading") && t.contains("- not a list")), "{shown:?}");
    assert!(!shown.iter().any(|t| t.contains("```") || t.contains("**")), "no markers are shown: {shown:?}");
}

#[test]
fn a_numbered_list_with_a_nested_bullet_list_renders_as_ol_li_ul() {
    let text = "1. first\n   - inner a\n   - inner b\n2. second\n\n- loose\n* also";
    let selectors = [".markdown ol > li", ".markdown ol > li > ul", ".markdown ol > li > ul > li", ".markdown > ul > li"];
    let Some(out) = detail(text, &selectors) else { return };
    assert_eq!(count(&out, selectors[0]), 2, "{out}");
    assert_eq!(count(&out, selectors[1]), 1, "the nested list sits inside the first item: {out}");
    assert_eq!(count(&out, selectors[2]), 2, "{out}");
    assert_eq!(count(&out, selectors[3]), 2, "- and * both make a bullet list: {out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == "inner a") && shown.iter().any(|t| t == "second"), "{shown:?}");
}

#[test]
fn long_words_wrap_and_fenced_code_scrolls_inside_its_own_box() {
    let css = std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let rule = |selector: &str| -> String {
        let at = css.find(selector).unwrap_or_else(|| panic!("app.css has no {selector} rule"));
        let body = &css[at..];
        body[..body.find('}').unwrap()].to_string()
    };
    assert!(rule(".markdown {").contains("overflow-wrap: anywhere"), "a long path wraps instead of widening the page");
    let pre = rule(".markdown pre {");
    assert!(pre.contains("overflow-x: auto"), "fenced code scrolls inside its box: {pre}");
    assert!(pre.contains("monospace"), "{pre}");
    assert!(css.contains(".markdown h3") && css.contains(".markdown h4"), "headings are sized for the page");
}

#[test]
fn a_heading_and_a_paragraph_render_as_an_h3_and_a_paragraph() {
    let Some(out) = detail("## Problem\n\nsome text", &[".markdown h3", ".markdown p"]) else { return };
    assert_eq!(count(&out, ".markdown h3"), 1, "{out}");
    assert_eq!(count(&out, ".markdown p"), 1, "{out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == "Problem"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "some text"), "{shown:?}");
    assert!(!shown.iter().any(|t| t.contains("##")), "the marker is not shown: {shown:?}");
}
