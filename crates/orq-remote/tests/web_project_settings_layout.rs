//! The "Starting runs" card of a project's settings page lays out like the other settings rows, measured
//! on the built page in a real headless Chromium: each setting is its own row with the label on the left and
//! the value (or, while editing, its control) beside or under it, never overlapping, never outside the card,
//! with the free-disk message under its input.

mod web_chromium;

use serde_json::{json, Value};
use std::path::Path;
use web_chromium::{get, measure, Reply};

const URL: &str = "/projects/demo/settings";
const CARD: &str = "[data-starting-runs]";
const LABELS: &str = "[data-starting-runs] [data-setting-label]";
const VALUES: &str = "[data-starting-runs] [data-setting-value]";
const ROWS: &str = "[data-starting-runs] [data-setting-row]";

fn replies(max: Value, floor: Value) -> Vec<Reply> {
    let project = json!({"id": "demo", "name": "demo", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let settings = json!({"settings": {
        "implementer": {"project": null, "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}},
        "reviewer": {"project": null, "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}},
        "delivery": "review",
        "max_active_runs": max,
        "min_free_disk_gb": floor,
        "queue_hours": null,
        "queue_stop_on_failure": true,
        "revision": 1
    }});
    vec![
        get("/api/projects", json!({"projects": [project]})),
        get("/api/projects/demo/settings", settings),
        get("/api/settings", json!({"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}})),
        get("/api/settings/options", json!({"options": [{"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "medium", "high"]}]})),
    ]
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

fn rects(result: &Value, selector: &str) -> Vec<Rect> {
    result["boxes"][selector]
        .as_array()
        .unwrap_or_else(|| panic!("no boxes for {selector}: {result}"))
        .iter()
        .map(|b| Rect { left: b["left"].as_i64().unwrap(), top: b["top"].as_i64().unwrap(), right: b["right"].as_i64().unwrap(), bottom: b["bottom"].as_i64().unwrap() })
        .collect()
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// `inner` lies inside `outer`, shrunk by `inset` on the left and right.
fn inside(inner: Rect, outer: Rect, inset: i64) -> bool {
    inner.left >= outer.left + inset && inner.right <= outer.right - inset && inner.top >= outer.top && inner.bottom <= outer.bottom
}

fn load(chromium: &Path, width: u32, replies: &[Reply], mut steps: Vec<Value>, measured: &[&str]) -> Value {
    steps.push(json!({"wait": CARD}));
    for selector in measured {
        steps.push(json!({"boxes": selector}));
    }
    let out = measure(chromium, width, URL, replies, Value::Array(steps.clone()));
    let result = out.result;
    assert!(result.get("error").is_none(), "{width} px, steps {steps:?}: {result}\nunanswered: {:?}", out.unanswered);
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{width} px: the page scrolls sideways: {result}");
    assert_eq!(result["offenders"], json!([]), "{width} px: text runs past the screen: {result}");
    result
}

fn assert_viewing(chromium: &Path, width: u32, max: Value, floor: Value, shown: [&str; 2]) {
    let result = load(chromium, width, &replies(max, floor), vec![json!({"wait": "[data-project-min-free-disk]"})], &[CARD, ROWS, LABELS, VALUES]);
    let card = rects(&result, CARD)[0];
    let (rows, labels, values) = (rects(&result, ROWS), rects(&result, LABELS), rects(&result, VALUES));
    assert_eq!((rows.len(), labels.len(), values.len()), (2, 2, 2), "{width} px: two rows, each with a label and a value: {result}");
    for at in 0..2 {
        assert!(!overlap(labels[at], values[at]), "{width} px: row {at}'s label {:?} and value {:?} overlap", labels[at], values[at]);
        for each in [rows[at], labels[at], values[at]] {
            assert!(inside(each, card, 0), "{width} px: {each:?} is outside the card {card:?}");
        }
        assert!(inside(labels[at], card, 12) && inside(values[at], card, 12), "{width} px: row {at}'s text is inside the card's padding: {:?} {:?} in {card:?}", labels[at], values[at]);
    }
    assert!(rows[1].top >= rows[0].bottom, "{width} px: the second row {:?} starts below the first {:?}", rows[1], rows[0]);
    let text = result["text"].as_str().unwrap();
    for expected in shown.iter().chain(["At most N active runs", "Free disk needed to start"].iter()) {
        assert!(text.contains(expected), "{width} px: {expected} is shown: {text}");
    }
}

#[test]
fn the_starting_runs_card_shows_each_limit_as_its_own_row_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390, 360] {
        assert_viewing(&chromium, width, json!(2), json!(15), ["At most 2 active runs", "15 GB"]);
    }
}

#[test]
fn the_starting_runs_card_without_limits_shows_no_limit_in_each_row() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390, 360] {
        assert_viewing(&chromium, width, Value::Null, Value::Null, ["No limit", "No limit"]);
    }
}

const SELECT: &str = "[data-starting-runs] [data-max-active-runs]";
const INPUT: &str = "[data-starting-runs] [data-min-free-disk]";
const MESSAGE: &str = "[data-starting-runs] [data-min-free-disk-invalid]";

fn editing(typed: Option<&str>) -> Vec<Value> {
    let mut steps = vec![json!({"click": "[data-project-settings-edit]"}), json!({"wait": INPUT})];
    if let Some(value) = typed {
        steps.push(json!({"type": INPUT, "value": value}));
        steps.push(json!({"wait": MESSAGE}));
    }
    steps
}

#[test]
fn while_editing_the_limit_select_and_the_free_disk_input_sit_inside_the_card_apart_from_their_labels() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let result = load(&chromium, width, &replies(json!(2), json!(15)), editing(None), &[CARD, LABELS, SELECT, INPUT]);
        let card = rects(&result, CARD)[0];
        let (labels, select, input) = (rects(&result, LABELS), rects(&result, SELECT)[0], rects(&result, INPUT)[0]);
        assert_eq!(labels.len(), 2, "{width} px: {result}");
        assert!(inside(select, card, 12) && inside(input, card, 12), "{width} px: the select {select:?} and input {input:?} are inset in the card {card:?}");
        assert!(!overlap(select, input), "{width} px: the select {select:?} and the input {input:?} overlap");
        for label in &labels {
            for control in [select, input] {
                assert!(!overlap(*label, control), "{width} px: the label {label:?} overlaps a control {control:?}");
            }
        }
        assert!(input.top >= select.bottom, "{width} px: the input {input:?} is in the row under the select {select:?}");
        if width == 1280 {
            assert!(input.right - input.left < (card.right - card.left) / 2, "{width} px: the input {input:?} is not the full card {card:?}");
        }
    }
}

#[test]
fn an_invalid_free_disk_value_shows_its_message_under_the_input_inside_the_card() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let result = load(&chromium, width, &replies(json!(2), json!(15)), editing(Some("abc")), &[CARD, INPUT, MESSAGE]);
        let (card, input, message) = (rects(&result, CARD)[0], rects(&result, INPUT)[0], rects(&result, MESSAGE)[0]);
        assert!(message.top >= input.bottom, "{width} px: the message {message:?} is under the input {input:?}");
        assert!(inside(message, card, 12), "{width} px: the message {message:?} is inside the card {card:?}");
    }
}

#[test]
fn the_select_and_the_input_keep_their_labels_as_accessible_names() {
    let Some(chromium) = web_chromium::binary() else { return };
    let named = ["label [data-max-active-runs][aria-label=\"At most N active runs\"]", "label [data-min-free-disk][aria-label=\"Free disk needed to start (GB)\"]"];
    let result = load(&chromium, 1280, &replies(json!(2), json!(15)), editing(None), &named);
    for selector in named {
        assert_eq!(rects(&result, selector).len(), 1, "{selector} is a labelled control: {result}");
    }
}
