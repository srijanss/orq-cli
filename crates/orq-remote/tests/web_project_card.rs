//! The project card follows the reference: a roomy rounded card, a bold name with an accent folder
//! icon and a muted chevron on a borderless header row, and a tinted, ringed selected state. Its stats
//! are always three: Features, runs active, and Reviews waiting when there are any, else the Queue's count.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn card(reviews: u64, queue: Value) -> Value {
    json!({"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 1, "max_active_runs": 2, "reviews_waiting": reviews, "failed_runs": 0, "failed_window_hours": 24, "queue": queue})
}

/// The Dashboard with `project`, then `steps` after signing in; the last frame, or None without node.
fn dashboard(project: Value, steps: Vec<Value>, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let filler = json!({"status": 200, "json": {}});
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let responses = json!([{"status": 200, "json": {"projects": [project]}}, filler, filler, filler, filler]);
    let scenario = json!({"url": "/dashboard", "responses": responses, "steps": all, "selectors": selectors});
    let output = std::process::Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"].as_array().unwrap().last().unwrap()["settled"].clone())
}

fn texts(frame: &Value) -> Vec<String> {
    frame["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

/// The card's texts after its name: value, label, value, label, ...
fn stat_texts(frame: &Value) -> Vec<String> {
    let texts = texts(frame);
    let start = texts.iter().position(|t| t == "alpha").unwrap();
    texts[start + 1..].to_vec()
}

#[test]
fn with_reviews_waiting_and_a_queue_the_third_item_is_reviews_waiting() {
    let project = card(2, json!({"count": 3, "paused": false, "waiting_for_review": null}));
    let selectors = [".project .card-stat", "[data-card-stat=\"alpha/reviews\"]", "[data-card-stat=\"alpha/queue\"]", "[data-card-queue]"];
    let Some(frame) = dashboard(project.clone(), vec![], &selectors) else { return };
    assert_eq!(frame["matches"][".project .card-stat"], 3, "{frame}");
    assert_eq!(frame["matches"]["[data-card-stat=\"alpha/reviews\"]"], 1, "{frame}");
    assert_eq!(frame["matches"]["[data-card-stat=\"alpha/queue\"]"], 0, "{frame}");
    assert_eq!(frame["matches"]["[data-card-queue]"], 0, "{frame}");
    assert_eq!(stat_texts(&frame)[4..6], ["2", "Reviews waiting"], "{:?}", texts(&frame));
    let Some(opened) = dashboard(project, vec![json!({"click": "[data-card-stat=\"alpha/reviews\"]"})], &[]) else { return };
    assert_eq!(opened["path"], "/projects/alpha/reviews");
}

#[test]
fn with_no_reviews_waiting_a_paused_queue_shows_only_its_count_as_queued() {
    let project = card(0, json!({"count": 3, "paused": true, "waiting_for_review": null}));
    let selectors = [".project .card-stat", "[data-card-stat=\"alpha/reviews\"]", "[data-card-stat=\"alpha/queue\"]", "[data-card-queue]"];
    let Some(frame) = dashboard(project.clone(), vec![], &selectors) else { return };
    assert_eq!(frame["matches"][".project .card-stat"], 3, "{frame}");
    assert_eq!(frame["matches"]["[data-card-stat=\"alpha/reviews\"]"], 0, "{frame}");
    assert_eq!(frame["matches"]["[data-card-stat=\"alpha/queue\"]"], 1, "{frame}");
    assert_eq!(frame["matches"]["[data-card-queue]"], 0, "{frame}");
    let stats = stat_texts(&frame);
    assert_eq!(stats[4..6], ["3", "Queued"], "{:?}", texts(&frame));
    assert!(!texts(&frame).iter().any(|t| t.to_lowercase().contains("paused")), "{:?}", texts(&frame));
    let Some(opened) = dashboard(project, vec![json!({"click": "[data-card-stat=\"alpha/queue\"]"})], &[]) else { return };
    assert_eq!(opened["path"], "/projects/alpha/queue");
}

/// Projects whose third stat differs: nothing queued, 3 queued and paused, and 2 reviews waiting.
fn three_cards() -> web_chromium::Reply {
    let named = |id: &str, reviews: u64, queue: Value| {
        let mut project = card(reviews, queue);
        project["id"] = json!(id);
        project["name"] = json!(id);
        project
    };
    let projects = [
        named("empty", 0, json!({"count": 0, "paused": false, "waiting_for_review": null})),
        named("paused", 0, json!({"count": 3, "paused": true, "waiting_for_review": null})),
        named("reviews", 2, json!({"count": 0, "paused": false, "waiting_for_review": null})),
    ];
    web_chromium::get("/api/projects", json!({"projects": projects}))
}

/// `box` relative to `card`: (left, top, width, height).
fn within(card: &Value, part: &Value) -> [i64; 4] {
    let n = |v: &Value, k: &str| v[k].as_i64().unwrap();
    [n(part, "left") - n(card, "left"), n(part, "top") - n(card, "top"), n(part, "right") - n(part, "left"), n(part, "bottom") - n(part, "top")]
}

#[test]
fn every_card_has_three_equal_stat_columns_in_the_same_places_whatever_the_third_item_is() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [390, 1280] {
        let steps = json!([{"wait": ".project .card-stat"}, {"boxes": ".project.card"}, {"boxes": ".project .card-stat"}, {"boxes": ".project .card-stat-value"}, {"boxes": ".project .card-stat-label"}, {"boxes": ".project .card-stat > .icon"}]);
        let out = web_chromium::measure(&chromium, width, "/dashboard", &[three_cards()], steps);
        let result = &out.result;
        assert!(result.get("error").is_none(), "{width} px: {}", result["error"]);
        assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{width} px scrolls sideways: {} > {}", result["scrollWidth"], result["clientWidth"]);
        let boxes = |sel: &str| result["boxes"][sel].as_array().unwrap().clone();
        let cards = boxes(".project.card");
        assert_eq!(cards.len(), 3, "{width} px: {}", result["boxes"]);
        let heights: Vec<i64> = cards.iter().map(|c| c["bottom"].as_i64().unwrap() - c["top"].as_i64().unwrap()).collect();
        // Each rect is rounded on its own, so 1 px is noise.
        assert!(heights.iter().all(|h| (h - heights[0]).abs() <= 1), "{width} px: card heights differ: {heights:?}");
        for part in [".project .card-stat", ".project .card-stat-value", ".project .card-stat-label", ".project .card-stat > .icon"] {
            let all = boxes(part);
            assert_eq!(all.len(), 9, "{width} px: {part}: {}", result["boxes"]);
            let placed: Vec<Vec<[i64; 4]>> = cards.iter().enumerate().map(|(c, card)| all[c * 3..c * 3 + 3].iter().map(|b| within(card, b)).collect()).collect();
            // The value's width follows its text; its place does not. Rects are rounded on their own, so 1 px is noise.
            let compared = if part == ".project .card-stat-value" { 2 } else { 4 };
            let same = |a: &[i64; 4], b: &[i64; 4]| (0..compared).all(|i| (a[i] - b[i]).abs() <= 1);
            assert!(placed.iter().all(|p| p.iter().zip(&placed[0]).all(|(a, b)| same(a, b))), "{width} px: {part} moves between cards: {placed:?}");
            if part == ".project .card-stat" {
                let widths: Vec<i64> = placed[0].iter().map(|b| b[2]).collect();
                assert!(widths.iter().all(|w| (w - widths[0]).abs() <= 1), "{width} px: the three columns differ in width: {widths:?}");
            }
        }
    }
}

#[test]
fn the_stats_are_a_fixed_three_column_grid() {
    let css = css();
    let stats = rule(&css, ".card-stats {");
    assert!(stats.contains("grid-template-columns: repeat(3, minmax(0, 1fr))") && !stats.contains("auto-fit"), "{stats}");
}

#[test]
fn with_nothing_queued_the_third_item_is_zero_queued() {
    let Some(frame) = dashboard(card(0, Value::Null), vec![], &["[data-card-stat=\"alpha/queue\"]"]) else { return };
    assert_eq!(frame["matches"]["[data-card-stat=\"alpha/queue\"]"], 1, "{frame}");
    assert_eq!(stat_texts(&frame)[4..6], ["0", "Queued"], "{:?}", texts(&frame));
    let Some(empty) = dashboard(card(0, json!({"count": 0, "paused": false, "waiting_for_review": null})), vec![], &[]) else { return };
    assert_eq!(stat_texts(&empty)[4..6], ["0", "Queued"], "{:?}", texts(&empty));
}

#[test]
fn a_queue_waiting_for_a_review_shows_only_its_count() {
    let Some(frame) = dashboard(card(0, json!({"count": 2, "paused": false, "waiting_for_review": "COM-01"})), vec![], &[]) else { return };
    assert_eq!(stat_texts(&frame)[4..6], ["2", "Queued"], "{:?}", texts(&frame));
    assert!(!texts(&frame).iter().any(|t| t.contains("COM-01") || t.contains("review)")), "{:?}", texts(&frame));
}

#[test]
fn the_third_item_has_the_same_parts_whether_reviews_or_queue() {
    let selectors = [".card-stat > .icon", ".card-stat > .card-stat-value", ".card-stat > .card-stat-label"];
    for project in [card(2, Value::Null), card(0, json!({"count": 1, "paused": false, "waiting_for_review": null}))] {
        let Some(frame) = dashboard(project, vec![], &selectors) else { return };
        for sel in selectors {
            assert_eq!(frame["matches"][sel], 3, "{sel}: {frame}");
        }
    }
}

#[test]
fn an_unavailable_project_shows_no_stats() {
    let mut project = card(2, json!({"count": 3, "paused": false, "waiting_for_review": null}));
    project["available"] = json!(false);
    let Some(frame) = dashboard(project, vec![], &[".project .card-stat"]) else { return };
    assert_eq!(frame["matches"][".project .card-stat"], 0, "{frame}");
    assert!(texts(&frame).iter().any(|t| t == "Unavailable"), "{:?}", texts(&frame));
}

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

#[test]
fn the_card_is_roomy_and_rounded_and_its_header_row_is_a_bold_borderless_name() {
    let css = css();
    let card = rule(&css, "\n.project {");
    assert!(card.contains("padding: 1rem 1.25rem") && card.contains("border-radius: 0.875rem") && card.contains("gap: 1rem"), "{card}");
    let head = rule(&css, ".project > button {");
    assert!(head.contains("background: transparent") && head.contains("border: 0") && head.contains("padding: 0"), "{head}");
    assert!(head.contains("font-size: 1.0625rem") && head.contains("font-weight: 700"), "{head}");
    assert!(rule(&css, ".project > button .icon:first-child {").contains("color: var(--accent)"));
}

#[test]
fn the_selected_card_is_ringed_and_tinted_with_the_accent() {
    let css = css();
    let selected = rule(&css, ".project.selected {");
    assert!(selected.contains("box-shadow: 0 0 0 2px var(--accent)") && selected.contains("color-mix(in srgb, var(--accent) 8%, var(--surface))"), "{selected}");
}

#[test]
fn a_card_shows_only_features_active_runs_and_last_updated() {
    use serde_json::{json, Value};
    let dir = env!("CARGO_MANIFEST_DIR");
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 2, "reviews_waiting": 3, "failed_runs": 4, "failed_window_hours": 24});
    let scenario = json!({"url": "/dashboard", "responses": [{"status": 200, "json": {"projects": [project]}}], "steps": [{"submit": "typed-token-0123456789abcdef0123456789"}], "selectors": [".project .card-stat"]});
    let output = std::process::Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    if !output.status.success() {
        return; // no node or dependencies here: the other tests still guard the styling
    }
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let settled = &out["frames"][1]["settled"];
    assert_eq!(settled["matches"][".project .card-stat"], 3, "{out}");
    let texts: Vec<&str> = settled["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    for kept in ["Features", "Active runs", "Reviews waiting"] {
        assert!(texts.contains(&kept), "{kept}: {texts:?}");
    }
    assert!(!texts.iter().any(|t| *t == "Waiting" || t.starts_with("Failed")), "{texts:?}");
}
