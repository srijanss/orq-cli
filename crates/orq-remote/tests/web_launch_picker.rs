//! With remote actions on, a pending feature's page offers "Start with…" beside Start, the popup's launch
//! picker: it reads the launch profiles, preselects the implementer and reviewer defaults, asks with the
//! picked pair and posts them to the start route; a refusal shows the server's reason.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 12] = [
    "[data-start-with]",
    "[data-start]",
    "[data-confirm]",
    "[data-cancel]",
    "[data-launch-implementer]",
    "[data-launch-reviewer]",
    "[data-launch-implementer] option[value=\"\"]",
    "[data-launch-reviewer] option[value=\"\"]",
    "[data-launch-implementer] option[value=\"\"]:checked",
    "[data-launch-reviewer] option[value=\"\"]:checked",
    IMPLEMENTER_FIELD,
    REVIEWER_FIELD,
];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger(actions: bool) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("sketch", "draft"), feature("next-one", "pending"), feature("done-one", "completed")], "remote_actions": actions}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn page(feature: &str, actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), runs(), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/features/{feature}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn profiles() -> Value {
    json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}})
}

#[test]
fn only_a_pending_feature_offers_start_with_and_only_with_actions_on() {
    let Some(out) = page("next-one", false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-start-with]"), 0, "{out}");
    let Some(out) = page("next-one", true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-start-with]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-start]"), 1, "Start with the defaults stays: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Start with…"), "{out}");
    for feature in ["sketch", "done-one"] {
        let Some(out) = page(feature, true, vec![], vec![]) else { return };
        assert_eq!(count(last(&out), "[data-start-with]"), 0, "{feature}: {out}");
    }
}

#[test]
fn the_picker_preselects_the_defaults_asks_with_the_picked_pair_and_posts_it() {
    let started = json!({"status": 202, "json": {"run": "run-7"}});
    let steps = vec![
        json!({"click": "[data-start-with]"}),
        json!({"set": "[data-launch-reviewer]", "value": "haiku"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = page("next-one", true, vec![profiles(), started], steps) else { return };
    let read = &out["requests"].as_array().unwrap().iter().find(|r| r["url"] == "/api/projects/alpha/launch-profiles").cloned().expect("the profiles are read");
    assert_eq!(read["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][2]).iter().any(|t| t == "Start next-one with opus as implementer and sonnet as reviewer?"), "the defaults are preselected: {out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Start next-one with opus as implementer and haiku as reviewer?"), "{out}");
    assert!(texts(&out["frames"][3]).iter().any(|t| t == "Yes, start"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/next-one/runs");
    assert_eq!(sent[0]["body"], r#"{"implementer":"opus","reviewer":"haiku"}"#);
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Started run run-7."), "{out}");
}

#[test]
fn a_refusal_shows_the_reason_no_profiles_says_so_and_cancel_sends_nothing() {
    let refused = json!({"status": 409, "json": {"error": "the profile haiku is not defined in the model profiles"}});
    let Some(out) = page("next-one", true, vec![profiles(), refused], vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "the profile haiku is not defined in the model profiles"), "{out}");
    assert_eq!(count(last(&out), "[data-start-with]"), 1, "the control is offered again: {out}");

    let none = json!({"status": 200, "json": {"profiles": [], "implementer": null, "reviewer": null}});
    let Some(out) = page("next-one", true, vec![none], vec![json!({"click": "[data-start-with]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "No model profiles are defined."), "{out}");
    assert_eq!(count(last(&out), "[data-launch-implementer]"), 0, "{out}");

    let Some(out) = page("next-one", true, vec![profiles()], vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-cancel]"})]) else { return };
    assert!(posts(&out).is_empty(), "cancelling sends nothing: {out}");
}

/// The project's agents as the server preselects them, and whether it offers Project default.
fn project_profiles(implementer: Value, reviewer: Value, project_default: bool) -> Value {
    json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": implementer, "reviewer": reviewer, "project_default": project_default}})
}

const IMPLEMENTER_DEFAULT: &str = "[data-launch-implementer] option[value=\"\"]";
const REVIEWER_DEFAULT: &str = "[data-launch-reviewer] option[value=\"\"]";
const IMPLEMENTER_DEFAULT_PICKED: &str = "[data-launch-implementer] option[value=\"\"]:checked";
const REVIEWER_DEFAULT_PICKED: &str = "[data-launch-reviewer] option[value=\"\"]:checked";

/// When no profile has the project's harness and model, both pickers offer Project default and start
/// on it; the question says so and the start names no profiles, so the run uses the project's agents.
#[test]
fn project_default_is_offered_and_preselected_and_starts_with_no_profiles_named() {
    let started = json!({"status": 202, "json": {"run": "run-8"}});
    let steps = vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = page("next-one", true, vec![project_profiles(json!(null), json!(null), true), started], steps) else { return };
    let asking = &out["frames"][2];
    for selector in [IMPLEMENTER_DEFAULT, REVIEWER_DEFAULT, IMPLEMENTER_DEFAULT_PICKED, REVIEWER_DEFAULT_PICKED] {
        assert_eq!(count(asking, selector), 1, "{selector}: {out}");
    }
    assert!(texts(asking).iter().any(|t| t == "Project default"), "{out}");
    assert!(texts(asking).iter().any(|t| t == "Start next-one with the project's default agents?"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/next-one/runs");
    assert_eq!(sent[0]["body"], "{}", "no --implementer / --reviewer: {out}");
    assert!(texts(last(&out)).iter().any(|t| t == "Started run run-8."), "{out}");
}

/// Project default applies to both roles, since a start names both profiles or neither: picking a
/// profile for one role gives the other its preselected profile, and picking Project default for one
/// role picks it for both.
#[test]
fn project_default_goes_for_both_roles_or_neither() {
    let started = json!({"status": 202, "json": {"run": "run-9"}});
    let steps = vec![
        json!({"click": "[data-start-with]"}),
        json!({"set": "[data-launch-implementer]", "value": "haiku"}),
        json!({"set": "[data-launch-reviewer]", "value": ""}),
        json!({"set": "[data-launch-reviewer]", "value": "opus"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = page("next-one", true, vec![project_profiles(json!(null), json!("sonnet"), true), started], steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert!(texts(&frames[3]).iter().any(|t| t == "Start next-one with haiku as implementer and sonnet as reviewer?"), "{out}");
    assert_eq!(count(&frames[3], REVIEWER_DEFAULT_PICKED), 0, "{out}");
    assert!(texts(&frames[4]).iter().any(|t| t == "Start next-one with the project's default agents?"), "{out}");
    assert_eq!(count(&frames[4], IMPLEMENTER_DEFAULT_PICKED), 1, "{out}");
    assert!(texts(&frames[5]).iter().any(|t| t == "Start next-one with haiku as implementer and opus as reviewer?"), "the implementer goes back to the first profile: {out}");
    assert_eq!(posts(&out)[0]["body"], r#"{"implementer":"haiku","reviewer":"opus"}"#, "{out}");
}

/// When a profile has each of the project's agents, or the project names none, there is no Project default.
#[test]
fn without_project_default_the_pickers_offer_only_profiles() {
    for reply in [profiles(), project_profiles(json!("sonnet"), json!("opus"), false)] {
        let Some(out) = page("next-one", true, vec![reply], vec![json!({"click": "[data-start-with]"})]) else { return };
        let frame = last(&out);
        assert_eq!(count(frame, IMPLEMENTER_DEFAULT) + count(frame, REVIEWER_DEFAULT), 0, "{out}");
        assert_eq!(count(frame, "[data-launch-implementer]"), 1, "{out}");
    }
}

/// With no profiles defined, a project with its own agents can still start with Project default, the only
/// choice either picker offers.
#[test]
fn with_no_profiles_project_default_is_the_only_choice_and_starts_with_no_profiles_named() {
    let only_default = json!({"status": 200, "json": {"profiles": [], "implementer": null, "reviewer": null, "project_default": true}});
    let started = json!({"status": 202, "json": {"run": "run-10"}});
    let steps = vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = page("next-one", true, vec![only_default, started], steps) else { return };
    let asking = &out["frames"][2];
    assert!(!texts(asking).iter().any(|t| t == "No model profiles are defined."), "{out}");
    assert_eq!(count(asking, IMPLEMENTER_DEFAULT_PICKED), 1, "{out}");
    assert_eq!(count(asking, REVIEWER_DEFAULT_PICKED), 1, "{out}");
    assert!(texts(asking).iter().any(|t| t == "Start next-one with the project's default agents?"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["body"], "{}", "{out}");
}

/// Each picker is a labelled field of the confirmation's form, as in the feature editor: its label above a
/// full-width select, both inside .confirm-fields ahead of the question.
const IMPLEMENTER_FIELD: &str = ".confirm > .confirm-fields > label.field > span.field-label + select[data-launch-implementer]";
const REVIEWER_FIELD: &str = ".confirm > .confirm-fields > label.field > span.field-label + select[data-launch-reviewer]";

#[test]
fn the_pickers_are_labelled_fields_of_the_confirmation_form() {
    let Some(out) = page("next-one", true, vec![profiles()], vec![json!({"click": "[data-start-with]"})]) else { return };
    let asking = last(&out);
    assert_eq!(count(asking, IMPLEMENTER_FIELD), 1, "{out}");
    assert_eq!(count(asking, REVIEWER_FIELD), 1, "{out}");
    assert!(texts(asking).iter().any(|t| t == "Implementer"), "{out}");
    assert!(texts(asking).iter().any(|t| t == "Reviewer"), "{out}");
}

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap()
}

/// The confirmation's fields form a grid, as the feature editor's do: one column on phones and two from
/// 720px, so the implementer and reviewer pickers line up side by side above the question.
#[test]
fn the_confirmation_fields_are_a_grid_of_one_column_then_two_from_720px() {
    let css = css();
    let grid = css.lines().find(|line| line.starts_with(".confirm-fields {")).expect("a .confirm-fields rule");
    assert!(grid.contains("display: grid") && grid.contains("grid-template-columns: 1fr;") && grid.contains("gap: 0.75rem 1rem"), "{grid}");
    let wide = css.lines().find(|line| line.starts_with("@media (min-width: 720px)") && line.contains(".confirm-fields {")).expect("a 720px .confirm-fields rule");
    assert!(wide.contains("grid-template-columns: 1fr 1fr"), "{wide}");
}
