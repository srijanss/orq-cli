//! The Queue pages follow the design system: each entry is a row with its position, its feature name, its
//! state as a `.badge` (with its text, and Next with an icon so it differs from Queued by more than colour)
//! and its reason under the name; a paused queue shows a banner with the pause icon, "Paused" and the
//! reason, and Resume. Rendered in jsdom for the markup and measured in a real headless Chromium for the
//! layout, at desktop and phone widths, and in both themes for the badge colours.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn entry(feature: &str, state: &str, reason: Value) -> Value {
    json!({"feature": feature, "state": state, "run": null, "reason": reason, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null})
}

fn queue(entries: Vec<Value>, paused: Value) -> Value {
    json!({"entries": entries, "count": entries.len(), "paused": paused})
}

/// One entry in every state the server names, and one in a state this page does not know.
fn every_state() -> Value {
    queue(
        vec![
            entry("COM-01", "running", Value::Null),
            entry("COM-02", "failed", Value::Null),
            entry("COM-03", "waiting", json!("waiting for COM-01")),
            entry("COM-04", "next", Value::Null),
            entry("COM-05", "queued", Value::Null),
            entry("COM-06", "parked", Value::Null),
        ],
        Value::Null,
    )
}

fn jsdom(url: &str, responses: Vec<Value>, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settled(out: &Value) -> &Value {
    &out["frames"].as_array().unwrap().last().unwrap()["settled"]
}

fn count(out: &Value, selector: &str) -> u64 {
    settled(out)["matches"][selector].as_u64().unwrap_or_else(|| panic!("no count for {selector}: {out}"))
}

fn texts(out: &Value) -> Vec<String> {
    settled(out)["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn project_json(id: &str) -> Value {
    json!({"id": id, "name": id, "available": true, "features": 6, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": null})
}

const BADGES: [(&str, &str); 5] = [("running", "badge-status-running"), ("failed", "badge-status-failed"), ("waiting", "badge-status-waiting"), ("next", "badge-status-queued"), ("queued", "badge-status-queued")];

fn badge_selectors(scope: &str) -> Vec<String> {
    let mut all: Vec<String> = BADGES.iter().map(|(state, class)| format!("{scope} [data-queue-state=\"{state}\"].badge.{class}")).collect();
    all.push(format!("{scope} [data-queue-state=\"next\"] svg"));
    all.push(format!("{scope} [data-queue-state=\"queued\"] svg"));
    all.push(format!("{scope} [data-queue-state=\"parked\"].badge"));
    all.push(format!("{scope} [data-queue-state=\"parked\"][class*=\"badge-status\"]"));
    all
}

fn assert_badges(out: &Value, scope: &str) {
    for (state, class) in BADGES {
        assert_eq!(count(out, &format!("{scope} [data-queue-state=\"{state}\"].badge.{class}")), 1, "{state} is a {class} badge: {out}");
    }
    assert_eq!(count(out, &format!("{scope} [data-queue-state=\"next\"] svg")), 1, "Next carries an icon: {out}");
    assert_eq!(count(out, &format!("{scope} [data-queue-state=\"queued\"] svg")), 0, "Queued has no icon, so the two differ by more than colour: {out}");
    assert_eq!(count(out, &format!("{scope} [data-queue-state=\"parked\"].badge")), 1, "an unknown state is still a badge: {out}");
    assert_eq!(count(out, &format!("{scope} [data-queue-state=\"parked\"][class*=\"badge-status\"]")), 0, "an unknown state's badge is neutral: {out}");
    let texts = texts(out);
    for shown in ["Running", "Failed", "Waiting", "Next", "Queued", "parked"] {
        assert!(texts.iter().any(|t| t == shown), "the badge's text {shown} is shown: {texts:?}");
    }
}

#[test]
fn each_state_is_a_badge_with_its_text_on_a_project_s_queue_and_on_the_all_projects_queue() {
    let scope = "[data-queue-section]";
    let selectors = badge_selectors(scope);
    let selectors: Vec<&str> = selectors.iter().map(String::as_str).collect();
    let listing = json!({"status": 200, "json": {"projects": [project_json("alpha")]}});
    let Some(out) = jsdom("/projects/alpha/queue", vec![json!({"status": 200, "json": {"queue": every_state(), "remote_actions": true}}), listing.clone()], &selectors) else { return };
    assert_badges(&out, scope);

    let scope = "[data-queue-group=\"alpha\"]";
    let selectors = badge_selectors(scope);
    let selectors: Vec<&str> = selectors.iter().map(String::as_str).collect();
    let Some(out) = jsdom("/queue", vec![listing, json!({"status": 200, "json": {"queue": every_state(), "remote_actions": true}})], &selectors) else { return };
    assert_badges(&out, scope);
}

const BANNER: [&str; 6] = [
    "[data-queue-section] > [data-queue-paused].banner.waiting-banner",
    "[data-queue-section] [data-queue-paused] > svg.icon",
    "[data-queue-section] [data-queue-paused] [data-queue-resume]",
    "[data-queue-group=\"alpha\"] > [data-queue-paused].banner.waiting-banner",
    "[data-queue-group=\"alpha\"] [data-queue-paused] > svg.icon",
    "[data-queue-group=\"alpha\"] [data-queue-resume]",
];

#[test]
fn a_paused_queue_shows_a_banner_with_the_pause_icon_paused_its_reason_and_resume() {
    let paused = queue(vec![entry("COM-01", "next", Value::Null)], json!("paused from the web"));
    let listing = json!({"status": 200, "json": {"projects": [project_json("alpha")]}});
    let reply = json!({"status": 200, "json": {"queue": paused, "remote_actions": true}});
    let Some(out) = jsdom("/projects/alpha/queue", vec![reply.clone(), listing.clone()], &BANNER) else { return };
    for selector in &BANNER[..3] {
        assert_eq!(count(&out, selector), 1, "{selector}: {out}");
    }
    assert!(texts(&out).iter().any(|t| t.contains("Paused: paused from the web")), "{:?}", texts(&out));

    let Some(out) = jsdom("/queue", vec![listing, reply], &BANNER) else { return };
    assert_eq!((count(&out, BANNER[3]), count(&out, BANNER[4]), count(&out, BANNER[5])), (1, 1, 0), "the banner, without Resume on the read-only page: {out}");
    assert!(texts(&out).iter().any(|t| t.contains("Paused: paused from the web")), "{:?}", texts(&out));
}

const LONG: &str = "queue-run-actions-review-resume-accept-and-merge-request";
const CARD: &str = "[data-queue-section]";
const ROWS: &str = "[data-queue-entry]";
const NAMES: &str = "[data-queue-entry] .queue-feature";
const STATES: &str = "[data-queue-entry] [data-queue-state]";
const REASONS: &str = "[data-queue-entry] .queue-reason";
const CONTROLS: &str = "[data-queue-entry] .queue-controls";

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl Rect {
    fn middle(self) -> i64 {
        (self.top + self.bottom) / 2
    }
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

fn inside(inner: Rect, outer: Rect) -> bool {
    inner.left >= outer.left && inner.right <= outer.right && inner.top >= outer.top && inner.bottom <= outer.bottom
}

fn project_queue(paused: Value) -> Value {
    queue(vec![entry(LONG, "waiting", json!("waiting for serve-systemd-user-service")), entry("serve-systemd-user-service", "next", Value::Null), entry("COM-03", "queued", Value::Null)], paused)
}

fn project_replies(paused: Value) -> Vec<web_chromium::Reply> {
    let project = json!({"id": "demo", "name": "demo", "available": true, "features": 3, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    vec![
        web_chromium::get("/api/projects", json!({"projects": [project]})),
        web_chromium::get("/api/projects/demo/queue", json!({"queue": project_queue(paused), "remote_actions": true})),
    ]
}

fn chromium_load(chromium: &std::path::Path, width: u32, url: &str, replies: &[web_chromium::Reply], wait: &str, measured: &[&str], init: &str) -> Value {
    let mut steps = vec![json!({"wait": wait})];
    for selector in measured {
        steps.push(json!({"boxes": selector}));
    }
    steps.push(json!({"lines": ".queue-feature", "text": LONG}));
    chromium_run(chromium, width, url, replies, steps, init)
}

/// Runs `steps` on `url` and checks the page fits the screen.
fn chromium_run(chromium: &std::path::Path, width: u32, url: &str, replies: &[web_chromium::Reply], steps: Vec<Value>, init: &str) -> Value {
    let out = web_chromium::measure_with(chromium, width, url, replies, Value::Array(steps.clone()), init);
    let result = out.result;
    assert!(result.get("error").is_none(), "{width} px, steps {steps:?}: {result}\nunanswered: {:?}", out.unanswered);
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{width} px: the page scrolls sideways: {result}");
    assert_eq!(result["offenders"], json!([]), "{width} px: text runs past the screen: {result}");
    result
}

#[test]
fn each_entry_of_a_project_s_queue_is_a_row_with_its_controls_in_one_place_at_every_width() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390, 360] {
        let desktop = width > 700;
        let result = chromium_load(&chromium, width, "/projects/demo/queue", &project_replies(Value::Null), "[data-queue-up]", &[CARD, ROWS, NAMES, STATES, REASONS, CONTROLS], "");
        let card = rects(&result, CARD)[0];
        let (rows, names, states, reasons, controls) = (rects(&result, ROWS), rects(&result, NAMES), rects(&result, STATES), rects(&result, REASONS), rects(&result, CONTROLS));
        assert_eq!((rows.len(), names.len(), states.len(), reasons.len(), controls.len()), (3, 3, 3, 1, 3), "{width} px: {result}");
        for at in 0..3 {
            let texts = if at == 0 { vec![names[at], states[at], reasons[0]] } else { vec![names[at], states[at]] };
            assert!(!overlap(names[at], states[at]), "{width} px: row {at}'s name {:?} and badge {:?} overlap", names[at], states[at]);
            assert!(!overlap(names[0], reasons[0]) && !overlap(states[0], reasons[0]), "{width} px: the reason {:?} overlaps the name or badge", reasons[0]);
            for each in texts.iter().chain([controls[at]].iter()) {
                assert!(inside(*each, rows[at]) && inside(*each, card), "{width} px: row {at}'s {each:?} is outside its row {:?} or the card {card:?}", rows[at]);
            }
            for text in &texts {
                assert!(!overlap(*text, controls[at]), "{width} px: row {at}'s {text:?} overlaps its controls {:?}", controls[at]);
            }
            for text in &texts {
                if desktop {
                    assert!(controls[at].left >= text.right, "{width} px: row {at}'s controls {:?} are right of its text {text:?}", controls[at]);
                } else {
                    assert!(controls[at].top >= text.bottom, "{width} px: row {at}'s controls {:?} are under its text {text:?}", controls[at]);
                }
            }
            if desktop {
                assert!((controls[at].middle() - rows[at].middle()).abs() <= 2, "{width} px: row {at}'s controls {:?} are centred in the row {:?}", controls[at], rows[at]);
            } else {
                assert!((controls[at].left - names[at].left).abs() <= 1, "{width} px: row {at}'s controls {:?} start where its name {:?} does", controls[at], names[at]);
            }
            if at > 0 {
                assert!(rows[at].top >= rows[at - 1].bottom, "{width} px: row {at} {:?} is below the one before {:?}", rows[at], rows[at - 1]);
                assert_eq!(controls[at].left, controls[0].left, "{width} px: every row's controls share one x: {controls:?}");
                assert_eq!(names[at].left, names[0].left, "{width} px: every row's name starts at one x: {names:?}");
            }
        }
        let lines = result["lines"][".queue-feature"].as_i64().unwrap();
        assert!((1..=3).contains(&lines), "{width} px: the long name takes {lines} lines, so it does not break at every hyphen: {result}");
    }
}

const BANNER_BOX: &str = "[data-queue-section] > [data-queue-paused]";
const BANNER_TEXT: &str = "[data-queue-paused] .queue-paused-text";
const RESUME: &str = "[data-queue-resume]";
const HEAD_TEXT: &str = "[data-queue-section] > .queue-head > p";
const PAUSE: &str = "[data-queue-pause]";

#[test]
fn the_paused_banner_tops_the_card_with_resume_beside_it_and_an_unpaused_head_keeps_pause_at_its_end() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390, 360] {
        let desktop = width > 700;
        let result = chromium_load(&chromium, width, "/projects/demo/queue", &project_replies(json!("paused from the web")), RESUME, &[CARD, ROWS, BANNER_BOX, BANNER_TEXT, RESUME], "");
        let (card, rows, banner, text, resume) = (rects(&result, CARD)[0], rects(&result, ROWS), rects(&result, BANNER_BOX)[0], rects(&result, BANNER_TEXT)[0], rects(&result, RESUME)[0]);
        for each in [banner, text, resume] {
            assert!(inside(each, card), "{width} px: {each:?} is inside the card {card:?}");
        }
        assert!(inside(text, banner) && inside(resume, banner), "{width} px: the text {text:?} and Resume {resume:?} are in the banner {banner:?}");
        assert!(banner.bottom <= rows[0].top, "{width} px: the banner {banner:?} is above the rows {:?}", rows[0]);
        assert!(!overlap(text, resume), "{width} px: the text {text:?} and Resume {resume:?} overlap");
        if desktop {
            assert!(resume.left >= text.right && resume.top < text.bottom && text.top < resume.bottom, "{width} px: Resume {resume:?} is beside the text {text:?}");
        } else {
            assert!(resume.top >= text.bottom, "{width} px: Resume {resume:?} is under the text {text:?}");
        }

        let result = chromium_load(&chromium, width, "/projects/demo/queue", &project_replies(Value::Null), PAUSE, &[CARD, HEAD_TEXT, PAUSE], "");
        let (card, text, pause) = (rects(&result, CARD)[0], rects(&result, HEAD_TEXT)[0], rects(&result, PAUSE)[0]);
        assert!(inside(text, card) && inside(pause, card) && !overlap(text, pause), "{width} px: the explanation {text:?} and Pause {pause:?} sit apart in the card {card:?}");
        if desktop {
            assert!(pause.left >= text.right && pause.top < text.bottom && text.top < pause.bottom, "{width} px: Pause {pause:?} is on the explanation's line {text:?}");
            assert!(card.right - pause.right <= 20, "{width} px: Pause {pause:?} is at the end of the card {card:?}");
        }
    }
}

const GROUP: &str = "[data-queue-group=\"demo\"]";
const GROUP_HEAD: &str = "[data-queue-group=\"demo\"] > .queue-group-head";
const GROUP_ROWS: &str = "[data-queue-group=\"demo\"] [data-queue-entry]";
const GROUP_NAMES: &str = "[data-queue-group=\"demo\"] [data-queue-entry] .queue-feature";
const GROUP_BADGES: &str = "[data-queue-group=\"demo\"] [data-queue-entry] .badge[data-queue-state]";
const GROUP_BANNER: &str = "[data-queue-group=\"demo\"] > [data-queue-paused]";

#[test]
fn the_all_projects_queue_shows_the_same_rows_with_badges_inside_each_card() {
    let Some(chromium) = web_chromium::binary() else { return };
    for width in [1280, 390] {
        let result = chromium_load(&chromium, width, "/queue", &project_replies(json!("paused from the web")), GROUP_ROWS, &[GROUP, GROUP_HEAD, GROUP_ROWS, GROUP_NAMES, GROUP_BADGES, GROUP_BANNER, "[data-queue-group] .queue-controls"], "");
        let (card, head, rows, names, badges, banner) = (rects(&result, GROUP)[0], rects(&result, GROUP_HEAD)[0], rects(&result, GROUP_ROWS), rects(&result, GROUP_NAMES), rects(&result, GROUP_BADGES), rects(&result, GROUP_BANNER)[0]);
        assert_eq!((rows.len(), names.len(), badges.len()), (3, 3, 3), "{width} px: three rows, each with its badge: {result}");
        assert!(rects(&result, "[data-queue-group] .queue-controls").is_empty(), "{width} px: the all-projects queue has no controls: {result}");
        for each in [head, banner].iter().chain(rows.iter()).chain(names.iter()).chain(badges.iter()) {
            assert!(inside(*each, card), "{width} px: {each:?} is inside the card {card:?}");
        }
        assert!(head.bottom <= banner.top && banner.bottom <= rows[0].top, "{width} px: the head {head:?}, then the banner {banner:?}, then the rows {:?}", rows[0]);
        for at in 0..3 {
            assert!(!overlap(names[at], badges[at]), "{width} px: row {at}'s name {:?} and badge {:?} overlap", names[at], badges[at]);
            if at > 0 {
                assert!(rows[at].top >= rows[at - 1].bottom && names[at].left == names[0].left, "{width} px: rows {rows:?} stack with their names at one x {names:?}");
            }
        }
    }
}

/// Starts dragging the first row and drags it over the third, as a person's drag does.
const DRAG: &str = r#"(function () {
  function go() {
    var rows = document.querySelectorAll("[data-queue-entry]");
    if (rows.length < 3) return setTimeout(go, 50);
    var data = new DataTransfer();
    rows[0].dispatchEvent(new DragEvent("dragstart", {bubbles: true, cancelable: true, dataTransfer: data}));
    setTimeout(function () {
      rows[2].dispatchEvent(new DragEvent("dragenter", {bubbles: true, cancelable: true, dataTransfer: data}));
      rows[2].dispatchEvent(new DragEvent("dragover", {bubbles: true, cancelable: true, dataTransfer: data}));
    }, 100);
  }
  setTimeout(go, 50);
})();"#;

#[test]
fn the_dragged_row_shows_as_dragged_and_the_row_it_would_drop_on_is_marked() {
    let Some(chromium) = web_chromium::binary() else { return };
    let dragging = "[data-queue-entry].dragging";
    let target = "[data-queue-entry].drop-target";
    let steps = vec![
        json!({"wait": dragging}),
        json!({"wait": target}),
        json!({"styles": dragging, "props": ["opacity", "outline-style", "outline-color"]}),
        json!({"styles": target, "props": ["box-shadow"]}),
        json!({"styles": "[data-queue-entry]:not(.dragging):not(.drop-target)", "props": ["opacity", "outline-style", "box-shadow"]}),
        json!({"styles": "[data-queue-entry]", "props": ["opacity"]}),
        json!({"tokens": ["--accent"]}),
    ];
    let result = chromium_run(&chromium, 1280, "/projects/demo/queue", &project_replies(Value::Null), steps, DRAG);
    let styles = |selector: &str| result["styles"][selector].as_array().unwrap_or_else(|| panic!("no styles for {selector}: {result}")).clone();
    let accent = result["tokens"]["--accent"].as_str().unwrap().to_string();
    let (dragged, marked, rest) = (styles(dragging), styles(target), styles("[data-queue-entry]:not(.dragging):not(.drop-target)"));
    assert_eq!((dragged.len(), marked.len(), rest.len()), (1, 1, 1), "one row dragged, one marked, one plain: {result}");
    assert!(dragged[0]["opacity"].as_str().unwrap().parse::<f64>().unwrap() < 1.0, "the dragged row is faded: {result}");
    assert_eq!((dragged[0]["outline-style"].as_str().unwrap(), dragged[0]["outline-color"].as_str().unwrap()), ("dashed", accent.as_str()), "the dragged row has a dashed accent outline: {result}");
    assert!(marked[0]["box-shadow"].as_str().unwrap().contains(&accent), "the row it would drop on is marked in the accent colour: {result}");
    assert_eq!((rest[0]["opacity"].as_str().unwrap(), rest[0]["outline-style"].as_str().unwrap(), rest[0]["box-shadow"].as_str().unwrap()), ("1", "none", "none"), "the other row is plain: {result}");
}

#[test]
fn in_both_themes_each_badge_takes_its_colours_from_the_status_tokens() {
    let Some(chromium) = web_chromium::binary() else { return };
    let project = json!({"id": "demo", "name": "demo", "available": true, "features": 6, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = vec![web_chromium::get("/api/projects", json!({"projects": [project]})), web_chromium::get("/api/projects/demo/queue", json!({"queue": every_state(), "remote_actions": true}))];
    let tones = [("running", "--status-running"), ("failed", "--status-failed"), ("waiting", "--status-waiting"), ("next", "--status-queued"), ("queued", "--status-queued")];
    let mut seen = Vec::new();
    for theme in ["light", "dark"] {
        let mut steps = vec![json!({"wait": "[data-queue-state=\"running\"]"})];
        for (state, _) in tones {
            steps.push(json!({"styles": format!("[data-queue-state=\"{state}\"]"), "props": ["color", "background-color"]}));
        }
        let tokens: Vec<String> = tones.iter().flat_map(|(_, token)| [token.to_string(), format!("{token}-bg")]).collect();
        steps.push(json!({"tokens": tokens}));
        let init = format!("try {{ localStorage.setItem(\"orq.theme\", \"{theme}\"); }} catch (e) {{}}");
        let result = chromium_run(&chromium, 1280, "/projects/demo/queue", &replies, steps, &init);
        for (state, token) in tones {
            let style = &result["styles"][format!("[data-queue-state=\"{state}\"]")][0];
            let (colour, background) = (result["tokens"][token].as_str().unwrap(), result["tokens"][format!("{token}-bg")].as_str().unwrap());
            assert_eq!((style["color"].as_str().unwrap(), style["background-color"].as_str().unwrap()), (colour, background), "{theme}: {state}'s badge uses {token}: {result}");
        }
        seen.push(result["tokens"]["--status-running"].as_str().unwrap().to_string());
    }
    assert_ne!(seen[0], seen[1], "the two themes were both measured: {seen:?}");
}
