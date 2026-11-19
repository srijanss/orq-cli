//! On a wide screen Add feature opens its form in a popup over the list: a dialog with a title and a close
//! button, which Escape or Close shuts without adding anything. Its question stays in the popup. On a phone the
//! form opens in the page, under the toolbar, as before.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = [
    ".feature-modal > .feature-modal-box[role=dialog][aria-modal=true] .feature-editor-fields [data-field-name]",
    ".feature-modal-box > .feature-modal-head > button[data-modal-close]",
    ".feature-modal",
    ".feature-modal-box .confirm [data-confirm]",
    "[data-feature-add]",
];

fn ledger() -> Value {
    json!({"status": 200, "json": {"features": [{"name": "f1", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": true}})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run(steps: Vec<Value>) -> Option<Value> {
    run_with(&SELECTORS, steps)
}

fn run_with(selectors: &[&str], steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger(), projects()], "steps": all, "selectors": selectors});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn add_feature_opens_a_dialog_with_its_question_inside_and_close_shuts_it() {
    let steps = vec![
        json!({"click": "[data-feature-add]"}),
        json!({"set": "[data-field-name]", "value": "new-one"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-cancel]"}),
        json!({"click": "[data-modal-close]"}),
    ];
    let Some(out) = run(steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[0]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "{out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 1, "the question stays in the popup: {out}");
    let last = frames.last().unwrap();
    assert_eq!(count(last, SELECTORS[2]), 0, "closed: {out}");
    assert_eq!(count(last, SELECTORS[4]), 1, "{out}");
    assert!(out["requests"].as_array().unwrap().iter().all(|r| r["method"] != "POST"), "nothing added: {out}");
}

#[test]
fn the_popup_is_over_the_page_on_a_wide_screen_and_in_the_page_on_a_phone() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let modal = css.lines().find(|line| line.starts_with(".feature-modal {")).expect("a .feature-modal rule");
    assert!(modal.contains("position: fixed") && modal.contains("inset: 0"), "{modal}");
    // On a phone only the Features page's own editor lies flat in its filters row; the Dashboard's popups keep
    // their box (web_todo_forms_consistency measures both in Chromium).
    let phone = css.split("@media (max-width: 700px) {").skip(1).map(|block| &block[..block.find("\n}").unwrap()]).find(|block| block.contains(".feature-modal")).expect("a phone rule for the popup");
    assert!(phone.contains(".feature-filters > .feature-editor .feature-modal, .feature-filters > .feature-editor .feature-modal-box { display: contents; }"), "{phone}");
    assert!(phone.contains(".feature-filters > .feature-editor .feature-modal-head { display: none; }"), "{phone}");
    assert!(!phone.contains("\n  .feature-modal, .feature-modal-box { display: contents; }"), "{phone}");
    // In a popup the box is the only card; the Features page's inline editor keeps the fields' frame on a phone.
    assert!(css.contains(".feature-modal-box .feature-editor-fields { max-width: none; padding: 0; border: 0; background: none; }"), "frameless in the popup");
    assert!(phone.contains(".feature-filters > .feature-editor .feature-modal-box .feature-editor-fields { max-width: 48rem; padding: 1rem; border: 1px solid var(--border); background: var(--surface); }"), "{phone}");
}

const NAME_INPUT: &str = "[data-field-name][autocapitalize=none][autocorrect=off][spellcheck=false][autocomplete=off][inputmode=text]";
const PROBLEM: &str = ".feature-editor-fields [data-name-problem][aria-live=polite]";
const SAVE_OFF: &str = "[data-feature-save]:disabled";

fn problem_text(frame: &Value) -> Option<String> {
    frame["settled"]["a11y"]["live"].as_array().unwrap().iter().find(|l| l["live"] == "polite" && l["text"].as_str().unwrap().contains("feature name")).map(|l| l["text"].as_str().unwrap().to_string())
}

#[test]
fn the_name_box_says_live_which_character_is_refused_and_save_waits_until_it_is_fixed() {
    let steps = vec![
        json!({"click": "[data-feature-add]"}),
        json!({"set": "[data-field-name]", "value": "a\u{2014}b"}),
        json!({"set": "[data-field-name]", "value": "a-b"}),
        json!({"set": "[data-field-name]", "value": ""}),
        json!({"click": "[data-feature-save]"}),
    ];
    let Some(out) = run_with(&[NAME_INPUT, PROBLEM, SAVE_OFF], steps) else { return };
    // Frame 0 is the sign-in.
    let frames = out["frames"].as_array().unwrap();
    let opened = &frames[2];
    assert_eq!(count(opened, NAME_INPUT), 1, "the phone keyboard leaves the name as typed: {out}");
    assert_eq!((count(opened, PROBLEM), count(opened, SAVE_OFF)), (0, 0), "an empty box says nothing yet: {out}");

    let dashed = &frames[3];
    assert_eq!((count(dashed, PROBLEM), count(dashed, SAVE_OFF)), (1, 1), "{out}");
    assert_eq!(problem_text(dashed).as_deref(), Some("\"\u{2014}\" (U+2014) at position 2 is not allowed in a feature name; use letters, digits, '-', '_' or '.' (use '-')"), "{out}");

    let fixed = &frames[4];
    assert_eq!((count(fixed, PROBLEM), count(fixed, SAVE_OFF)), (0, 0), "{out}");
    assert_eq!(count(&frames[5], PROBLEM), 0, "emptied, but Save not pressed yet: {out}");

    let pressed = &frames[6];
    assert_eq!(problem_text(pressed).as_deref(), Some("a feature name is required"), "{out}");
    assert_eq!(count(pressed, SAVE_OFF), 1, "{out}");
    assert!(out["requests"].as_array().unwrap().iter().all(|r| r["method"] != "POST"), "nothing sent: {out}");
}

fn ledger_with(names: &[&str]) -> Value {
    let features: Vec<Value> = names.iter().map(|name| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null})).collect();
    json!({"status": 200, "json": {"features": features, "remote_actions": true}})
}

/// Opens Add feature, types `typed` into the name, and (when `confirm`) presses Add and confirms.
fn typed_name(typed: &str, confirm: bool) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut steps = vec![json!({"submit": TOKEN}), json!({"click": "[data-feature-add]"}), json!({"set": "[data-field-name]", "value": typed})];
    if confirm {
        steps.push(json!({"click": "[data-feature-save]"}));
        steps.push(json!({"click": "[data-confirm]"}));
    }
    let added = json!({"status": 200, "json": {"feature": "added"}});
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger_with(&["f1", "done-one"]), projects(), added, ledger_with(&["f1", "done-one"])], "steps": steps, "selectors": [PROBLEM, SAVE_OFF, "[data-saved-as]", ".confirm-question"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn saved_as(frame: &Value) -> Option<String> {
    frame["settled"]["a11y"]["live"].as_array().unwrap().iter().find(|l| l["live"] == "polite" && l["text"].as_str().unwrap().starts_with("Saved as")).map(|l| l["text"].as_str().unwrap().to_string())
}

fn posted_name(out: &Value) -> Option<Value> {
    out["requests"].as_array().unwrap().iter().find(|r| r["method"] == "POST").map(|r| {
        let body: Value = serde_json::from_str(r["body"].as_str().unwrap()).unwrap();
        body["fields"]["name"].clone()
    })
}

#[test]
fn a_name_typed_with_spaces_is_shown_checked_and_sent_with_hyphens() {
    let Some(out) = typed_name("reviews show mr link", true) else { return };
    let typed = &out["frames"][3];
    assert_eq!(saved_as(typed).as_deref(), Some("Saved as reviews-show-mr-link"), "{out}");
    assert_eq!((count(typed, PROBLEM), count(typed, SAVE_OFF)), (0, 0), "no problem, Add enabled: {out}");
    let asked = &out["frames"][4];
    assert!(asked["settled"]["texts"].as_array().unwrap().iter().any(|t| t.as_str().unwrap().contains("Add reviews-show-mr-link?")), "{out}");
    assert_eq!(posted_name(&out), Some(json!("reviews-show-mr-link")), "{out}");
}

#[test]
fn spaces_at_the_ends_runs_of_spaces_tabs_and_non_breaking_spaces_become_one_hyphen() {
    for (typed, saved) in [("  a   b  ", "a-b"), ("a - b", "a-b"), ("a\tb", "a-b"), ("a\u{00a0}b", "a-b")] {
        let Some(out) = typed_name(typed, true) else { return };
        assert_eq!(saved_as(&out["frames"][3]).as_deref(), Some(format!("Saved as {saved}").as_str()), "{typed:?}: {out}");
        assert_eq!(posted_name(&out), Some(json!(saved)), "{typed:?}: {out}");
    }
}

#[test]
fn a_typed_name_that_converts_to_an_existing_feature_is_refused_as_a_duplicate() {
    let Some(out) = typed_name("done one", false) else { return };
    let typed = &out["frames"][3];
    assert_eq!(count(typed, SAVE_OFF), 1, "{out}");
    assert_eq!(problem_text(typed).as_deref(), Some("\"done-one\" is already a feature name in this project; choose another"), "{out}");
}

#[test]
fn a_character_the_rule_refuses_still_shows_its_problem_after_the_spaces_are_converted() {
    let Some(out) = typed_name("my feature!", false) else { return };
    let typed = &out["frames"][3];
    assert_eq!(count(typed, SAVE_OFF), 1, "{out}");
    assert!(problem_text(typed).unwrap_or_default().starts_with("\"!\" (U+0021) at position 11"), "{out}");
}

#[test]
fn a_name_typed_with_hyphens_shows_no_saved_as_hint_and_is_sent_unchanged() {
    let Some(out) = typed_name("already-hyphenated", true) else { return };
    assert_eq!(saved_as(&out["frames"][3]), None, "{out}");
    assert_eq!(count(&out["frames"][3], "[data-saved-as]"), 0, "{out}");
    assert_eq!(posted_name(&out), Some(json!("already-hyphenated")), "{out}");
}

#[test]
fn a_long_run_of_hyphens_is_converted_without_freezing_the_page() {
    // Each render converts the name, so a conversion that slows with the square of the length would hold
    // the page for seconds here (about five seconds per render at this length).
    let typed = format!("a{}b", "-".repeat(60_000));
    let started = std::time::Instant::now();
    let Some(out) = typed_name(&typed, false) else { return };
    let took = started.elapsed();
    assert_eq!(saved_as(&out["frames"][3]), None, "nothing to convert");
    assert_eq!(count(&out["frames"][3], SAVE_OFF), 0, "a valid name");
    assert!(took < std::time::Duration::from_secs(4), "took {took:?}");
}
