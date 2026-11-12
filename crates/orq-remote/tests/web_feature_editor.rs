//! With remote actions on, a feature's page offers Edit and the Features list offers Add feature, the popup's
//! editor as plain inputs (no `<form>`). Edit reads the feature's fields and the ledger's version first; Save
//! asks, then posts `{fields, version}`. Refused values keep the editor open with the reason, and a ledger
//! changed since it was read says so with a Reload button.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 12] = [
    "[data-feature-edit]",
    "[data-feature-add]",
    "[data-field-name]",
    "[data-field-description]",
    "[data-field-test-file]",
    "[data-field-target-files]",
    "[data-field-parallelizable]",
    "[data-field-depends]",
    "[data-feature-save]",
    "[data-confirm]",
    "[data-cancel]",
    "[data-feature-reload]",
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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger(actions: bool) -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("sketch", "draft"), feature("next-one", "pending"), feature("done-one", "completed")], "remote_actions": actions}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn fields() -> Value {
    json!({"status": 200, "json": {"fields": {"name": "next-one", "description": "One thing", "depends_on": [], "parallelizable": null, "test_file": "tests/one.rs", "target_files": ["src/one.rs"]}, "version": "v1"}})
}

fn detail(feature: &str, actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), runs(), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/features/{feature}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn list(actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), projects()];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/features", "responses": responses, "steps": all, "selectors": SELECTORS}))
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

#[test]
fn edit_and_add_are_offered_only_with_actions_on() {
    let Some(out) = detail("next-one", false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-feature-edit]"), 0, "{out}");
    let Some(out) = list(false, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-feature-add]"), 0, "{out}");
    for feature in ["sketch", "next-one", "done-one"] {
        let Some(out) = detail(feature, true, vec![], vec![]) else { return };
        assert_eq!(count(last(&out), "[data-feature-edit]"), 1, "{feature}: {out}");
        assert_eq!(count(last(&out), "[data-field-description]"), 0, "the editor opens only on Edit: {out}");
    }
    let Some(out) = list(true, vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-feature-add]"), 1, "{out}");
}

#[test]
fn edit_reads_the_fields_then_asks_and_posts_them_with_the_version_read() {
    let saved = json!({"status": 200, "json": {"feature": "next-one"}});
    let steps = vec![
        json!({"click": "[data-feature-edit]"}),
        json!({"set": "[data-field-description]", "value": "Reworded"}),
        json!({"set": "[data-field-target-files]", "value": "src/a.rs, src/b.rs"}),
        json!({"click": "[data-field-depends=\"sketch\"]"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = detail("next-one", true, vec![fields(), saved], steps) else { return };
    let read = out["requests"].as_array().unwrap().iter().find(|r| r["url"] == "/api/projects/alpha/features/next-one/fields").cloned().expect("the fields are read");
    assert_eq!(read["method"], "GET");
    assert_eq!(read["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let opened = &out["frames"][2];
    assert_eq!(count(opened, "[data-field-name]"), 0, "an edit cannot rename: {out}");
    assert_eq!(count(opened, "[data-field-depends]"), 2, "every other feature can be a dependency: {out}");
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Save next-one?"), "{out}");
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Yes, save"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/next-one");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"fields": {"description": "Reworded", "depends_on": ["sketch"], "target_files": ["src/a.rs", "src/b.rs"]}, "version": "v1"}), "only what was changed is sent");
    assert!(texts(&out["frames"][7]).iter().any(|t| t == "Saved next-one."), "{out}");
}

#[test]
fn add_asks_and_posts_the_new_feature_to_the_ledger() {
    let added = json!({"status": 200, "json": {"feature": "brand-new"}});
    let steps = vec![
        json!({"click": "[data-feature-add]"}),
        json!({"set": "[data-field-name]", "value": "brand-new"}),
        json!({"set": "[data-field-description]", "value": "New"}),
        json!({"set": "[data-field-parallelizable]", "value": "yes"}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = list(true, vec![added], steps) else { return };
    assert_eq!(count(&out["frames"][2], "[data-field-depends]"), 3, "{out}");
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Add brand-new?"), "{out}");
    assert!(texts(&out["frames"][6]).iter().any(|t| t == "Yes, add"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features");
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"fields": {"name": "brand-new", "description": "New", "depends_on": [], "parallelizable": true, "test_file": null, "target_files": []}}));
    assert!(texts(&out["frames"][7]).iter().any(|t| t == "Added brand-new."), "{out}");
}

#[test]
fn refused_values_keep_the_editor_a_conflict_offers_reload_and_cancel_sends_nothing() {
    let invalid = json!({"status": 422, "json": {"error": "feature \"next-one\" depends on itself"}});
    let save = vec![json!({"click": "[data-feature-edit]"}), json!({"click": "[data-feature-save]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = detail("next-one", true, vec![fields(), invalid], save.clone()) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "feature \"next-one\" depends on itself"), "{out}");
    assert_eq!(count(last(&out), "[data-field-description]"), 1, "the editor stays open: {out}");
    assert_eq!(count(last(&out), "[data-feature-reload]"), 0, "{out}");

    let changed = json!({"status": 409, "json": {"error": "The ledger changed since you opened it; nothing was written"}});
    let Some(out) = detail("next-one", true, vec![fields(), changed], save) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "The ledger changed since you opened it; nothing was written"), "{out}");
    assert_eq!(count(last(&out), "[data-feature-reload]"), 1, "{out}");

    let gone = json!({"status": 404, "json": {"error": "not found"}});
    let Some(out) = detail("next-one", true, vec![gone], vec![json!({"click": "[data-feature-edit]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "not found"), "{out}");
    assert_eq!(count(last(&out), "[data-field-description]"), 0, "{out}");

    let cancel = vec![json!({"click": "[data-feature-edit]"}), json!({"click": "[data-feature-save]"}), json!({"click": "[data-cancel]"}), json!({"click": "[data-cancel]"})];
    let Some(out) = detail("next-one", true, vec![fields()], cancel) else { return };
    assert!(posts(&out).is_empty(), "cancelling sends nothing: {out}");
    assert_eq!(count(last(&out), "[data-field-description]"), 0, "the second Cancel closes the editor: {out}");
    assert_eq!(count(last(&out), "[data-feature-edit]"), 1, "{out}");
}

#[test]
fn an_empty_ledger_still_offers_add_feature() {
    let empty = json!({"status": 200, "json": {"features": [], "remote_actions": true}});
    let Some(out) = run(json!({"url": "/projects/alpha/features", "responses": [empty, projects()], "steps": [{"submit": TOKEN}, {"click": "[data-feature-add]"}], "selectors": SELECTORS})) else { return };
    assert!(texts(&out["frames"][1]).iter().any(|t| t == "No features in this project's ledger."), "{out}");
    assert_eq!(count(last(&out), "[data-field-name]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-field-depends]"), 0, "nothing to depend on: {out}");
}

#[test]
fn an_edit_sends_an_emptied_box_as_a_clear_and_leaves_out_what_was_not_touched() {
    let saved = json!({"status": 200, "json": {"feature": "next-one"}});
    let steps = vec![
        json!({"click": "[data-feature-edit]"}),
        json!({"set": "[data-field-description]", "value": "  "}),
        json!({"set": "[data-field-test-file]", "value": ""}),
        json!({"set": "[data-field-target-files]", "value": ""}),
        json!({"click": "[data-feature-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = detail("next-one", true, vec![fields(), saved], steps) else { return };
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    let body: Value = serde_json::from_str(sent[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"fields": {"description": "", "test_file": "", "target_files": []}, "version": "v1"}));
}
