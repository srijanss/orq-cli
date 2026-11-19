//! The "Queue" card of a project's settings page lays out like "Starting runs", measured on the built page
//! in a real headless Chromium: each setting is its own row with the label on the left and the value (or,
//! while editing, its control) beside or under it, never overlapping, never outside the card; the hours
//! message sits under its input, and the failure setting is a checkbox with its text beside it.

mod web_chromium;

use serde_json::{json, Value};
use std::path::Path;
use web_chromium::{get, measure, Reply};

const URL: &str = "/projects/demo/settings";
const CARD: &str = "[data-queue-settings]";
const LABELS: &str = "[data-queue-settings] [data-setting-label]";
const VALUES: &str = "[data-queue-settings] [data-setting-value]";
const ROWS: &str = "[data-queue-settings] [data-setting-row]";

fn replies(hours: Value, stop: bool) -> Vec<Reply> {
    let project = json!({"id": "demo", "name": "demo", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let settings = json!({"settings": {
        "implementer": {"project": null, "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}},
        "reviewer": {"project": null, "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}},
        "delivery": "review",
        "max_active_runs": 2,
        "min_free_disk_gb": 15,
        "queue_hours": hours,
        "queue_stop_on_failure": stop,
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

fn assert_viewing(chromium: &Path, width: u32, hours: Value, stop: bool, shown: [&str; 2]) {
    let result = load(chromium, width, &replies(hours, stop), vec![json!({"wait": "[data-project-queue-stop]"})], &[CARD, ROWS, LABELS, VALUES]);
    let card = rects(&result, CARD)[0];
    let (rows, labels, values) = (rects(&result, ROWS), rects(&result, LABELS), rects(&result, VALUES));
    assert_eq!((rows.len(), labels.len(), values.len()), (4, 4, 4), "{width} px: four rows, each with a label and a value: {result}");
    for at in 0..4 {
        assert!(!overlap(labels[at], values[at]), "{width} px: row {at}'s label {:?} and value {:?} overlap", labels[at], values[at]);
        for each in [rows[at], labels[at], values[at]] {
            assert!(inside(each, card, 0), "{width} px: {each:?} is outside the card {card:?}");
        }
        assert!(inside(labels[at], card, 12) && inside(values[at], card, 12), "{width} px: row {at}'s text is inside the card's padding: {:?} {:?} in {card:?}", labels[at], values[at]);
    }
    for at in 1..4 {
        assert!(rows[at].top >= rows[at - 1].bottom, "{width} px: row {at} {:?} starts below the one before {:?}", rows[at], rows[at - 1]);
        assert!(labels[at].top >= labels[at - 1].bottom && labels[at].top >= values[at - 1].bottom, "{width} px: row {at}'s label {:?} is below the row before {:?} {:?}", labels[at], labels[at - 1], values[at - 1]);
    }
    let text = result["text"].as_str().unwrap();
    for expected in shown.iter().chain(["Hours the queue starts runs", "When a run fails", "Wait for each run's review (and MR) before the next", "Pull the target after an MR merges and before each run"].iter()) {
        assert!(text.contains(expected), "{width} px: {expected} is shown: {text}");
    }
}

#[test]
fn the_queue_card_shows_each_setting_as_its_own_row_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390, 360] {
        assert_viewing(&chromium, width, Value::Null, false, ["Any time", "Keeps going; only its dependents wait"]);
        assert_viewing(&chromium, width, json!("22:00-07:00"), true, ["22:00-07:00", "Pauses the queue"]);
    }
}

const INPUT: &str = "[data-queue-settings] [data-queue-hours]";
const MESSAGE: &str = "[data-queue-settings] [data-queue-hours-invalid]";
const CHECKBOX: &str = "[data-queue-settings] [data-queue-stop-on-failure]";
const CHECKBOX_TEXT: &str = "[data-queue-settings] label:has(> [data-queue-stop-on-failure]) > span";

fn editing(typed: Option<&str>) -> Vec<Value> {
    let mut steps = vec![json!({"click": "[data-project-settings-edit]"}), json!({"wait": INPUT})];
    if let Some(value) = typed {
        steps.push(json!({"type": INPUT, "value": value}));
        steps.push(json!({"wait": MESSAGE}));
    }
    steps
}

#[test]
fn while_editing_the_hours_input_and_the_checkbox_sit_inside_the_card_apart_from_their_labels() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let result = load(&chromium, width, &replies(json!("22:00-07:00"), true), editing(None), &[CARD, LABELS, INPUT, CHECKBOX, CHECKBOX_TEXT]);
        let card = rects(&result, CARD)[0];
        let (labels, input, checkbox, text) = (rects(&result, LABELS), rects(&result, INPUT)[0], rects(&result, CHECKBOX)[0], rects(&result, CHECKBOX_TEXT));
        assert_eq!((labels.len(), text.len()), (1, 1), "{width} px: the hours label, and the checkbox's text beside it: {result}");
        let (label, text) = (labels[0], text[0]);
        for each in [input, checkbox, text, label] {
            assert!(inside(each, card, 12), "{width} px: {each:?} is inset in the card {card:?}");
        }
        assert!(!overlap(label, input), "{width} px: the label {label:?} overlaps the input {input:?}");
        for other in [label, input] {
            assert!(!overlap(other, checkbox) && !overlap(other, text), "{width} px: {other:?} overlaps the checkbox {checkbox:?} or its text {text:?}");
        }
        assert!(checkbox.top >= input.bottom && text.top >= input.bottom, "{width} px: the checkbox row {checkbox:?} {text:?} is under the input {input:?}");
        assert!(text.left >= checkbox.right, "{width} px: the text {text:?} is right of the checkbox {checkbox:?}");
        let (middle, text_middle) = ((checkbox.top + checkbox.bottom) / 2, (text.top + text.bottom) / 2);
        assert!(middle >= text.top && middle <= text.bottom && text_middle >= checkbox.top - 4 && text_middle <= checkbox.bottom + 4, "{width} px: the checkbox {checkbox:?} sits on its text's line {text:?}");
        assert!(text.bottom - text.top < 2 * (checkbox.bottom - checkbox.top) + 8, "{width} px: the checkbox's text {text:?} stays on one line");
        if width == 1280 {
            assert!(input.right - input.left < (card.right - card.left) / 2, "{width} px: the input {input:?} is not the full card {card:?}");
        }
    }
}

#[test]
fn an_invalid_window_shows_its_message_under_the_input_inside_the_card() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let result = load(&chromium, width, &replies(Value::Null, false), editing(Some("22:00")), &[CARD, INPUT, MESSAGE, CHECKBOX]);
        let (card, input, message, checkbox) = (rects(&result, CARD)[0], rects(&result, INPUT)[0], rects(&result, MESSAGE)[0], rects(&result, CHECKBOX)[0]);
        assert!(message.top >= input.bottom, "{width} px: the message {message:?} is under the input {input:?}");
        assert!(inside(message, card, 12), "{width} px: the message {message:?} is inside the card {card:?}");
        assert!(!overlap(message, checkbox) && checkbox.top >= message.bottom, "{width} px: the checkbox {checkbox:?} stays below the message {message:?}");
    }
}

#[test]
fn the_input_and_the_checkbox_keep_their_names_and_clicking_the_text_toggles_the_checkbox() {
    let Some(chromium) = web_chromium::binary() else { return };
    let hours = "label [data-queue-hours][aria-label=\"Hours the queue starts runs\"]";
    let unchecked = "[data-queue-settings] [data-queue-stop-on-failure]:not(:checked)";
    let checked = "[data-queue-settings] [data-queue-stop-on-failure]:checked";
    let mut steps = editing(None);
    steps.push(json!({"click": CHECKBOX_TEXT, "text": "A failed run pauses the queue"}));
    steps.push(json!({"wait": checked}));
    let result = load(&chromium, 1280, &replies(Value::Null, false), steps, &[hours, CHECKBOX_TEXT, checked, unchecked]);
    assert_eq!(rects(&result, hours).len(), 1, "the hours input is named by its label: {result}");
    assert_eq!(rects(&result, CHECKBOX_TEXT).len(), 1, "the checkbox is named by the text in its label: {result}");
    assert_eq!((rects(&result, checked).len(), rects(&result, unchecked).len()), (1, 0), "clicking the text turned the checkbox on: {result}");
}
