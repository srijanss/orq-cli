//! A project's Settings page, at /projects/{id}/settings: its implementer and reviewer (harness, model,
//! effort), each "Machine default (...)" while the project leaves it unset, and "When a run finishes"
//! (Open review / Push only / Merge locally). Edit, while remote actions are on, offers the machine's
//! options with a reset to the machine default per role, and Save asks, then sends the whole record
//! with the revision it was read at; a stale revision shows the server's reason and offers Reload.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const URL: &str = "/projects/alpha/settings";
const SELECTORS: [&str; 9] = [
    "[data-project-settings-edit]",
    "[data-role-harness]",
    "[data-role-model]",
    "[data-role-reset]",
    "[data-delivery]",
    "[data-project-settings-save]",
    "[data-confirm]",
    "[data-settings-reload]",
    "[data-cancel]",
];

fn run(responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps.as_array().unwrap().iter().cloned());
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": URL, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}})
}

/// The alpha project's settings: its own implementer, the reviewer left to the machine, and `delivery`.
fn project_settings(delivery: &str, revision: u64) -> Value {
    json!({"status": 200, "json": {"settings": {
        "implementer": {
            "project": {"harness": "claude", "model": "sonnet", "effort": "medium"},
            "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}
        },
        "reviewer": {"project": null, "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}},
        "delivery": delivery,
        "revision": revision
    }}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn a_projects_settings_page_shows_its_agents_the_machine_default_for_an_unset_role_and_when_a_run_finishes() {
    let Some(out) = run(json!([project_settings("merge_local", 3), projects()]), json!([])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects/alpha/settings", "{out}");
    let texts = shown(last(&out));
    for expected in ["Implementer", "claude sonnet, effort medium", "Reviewer", "Machine default (pi kimi)", "When a run finishes", "Merge locally"] {
        assert!(texts.iter().any(|t| t == expected), "{expected}: {texts:?}");
    }
    assert!(!texts.iter().any(|t| t.contains("not available yet")), "{texts:?}");
}

/// The machine's settings as /api/settings gives them, with remote actions `on`.
fn machine_settings(on: bool) -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

fn options() -> Value {
    json!({"status": 200, "json": {"options": [
        {"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "medium", "high"]},
        {"harness": "pi", "models": ["kimi", "glm"]}
    ]}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn body(request: &Value) -> Value {
    serde_json::from_str(request["body"].as_str().unwrap()).unwrap()
}

#[test]
fn edit_resets_a_role_to_the_machine_default_picks_another_and_save_asks_then_sends_the_record_with_its_revision() {
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = json!([
        {"click": "[data-project-settings-edit]"},
        {"click": "[data-role-reset=\"implementer\"]"},
        {"set": "[data-role-harness=\"reviewer\"]", "value": "pi"},
        {"set": "[data-delivery]", "value": "push"},
        {"click": "[data-project-settings-save]"},
        {"click": "[data-confirm]"},
    ]);
    let responses = json!([project_settings("merge_local", 3), projects(), machine_settings(true), options(), ok, project_settings("push", 4)]);
    let Some(out) = run(responses, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], "[data-project-settings-edit]"), 1, "{out}");
    assert_eq!(count(&frames[2], "[data-role-harness]"), 2, "each role offers a harness: {out}");
    assert_eq!(count(&frames[2], "[data-role-model]"), 1, "only the role the project sets has a model: {out}");
    assert_eq!(count(&frames[3], "[data-role-model]"), 0, "the reset leaves the implementer to the machine: {out}");
    assert_eq!(count(&frames[4], "[data-role-model]"), 1, "the reviewer now has a model: {out}");
    assert!(shown(&frames[6]).iter().any(|t| t == "Save this project's settings?"), "Save asks first: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/settings");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(body(&sent[0]), json!({"implementer": null, "reviewer": {"harness": "pi", "model": "kimi", "effort": null}, "delivery": "push", "revision": 3}));
    let reads = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/projects/alpha/settings" && r["method"] == "GET").count();
    assert_eq!(reads, 2, "the settings are read again after a save: {out}");
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Push only"), "{out}");
    assert_eq!(count(frame, "[data-role-harness]"), 0, "the editor closes: {out}");
}

#[test]
fn a_stale_revision_shows_the_servers_reason_and_reload_reads_the_settings_again() {
    let stale = json!({"status": 409, "json": {"error": "The project's settings changed since they were read"}});
    let steps = json!([
        {"click": "[data-project-settings-edit]"},
        {"set": "[data-delivery]", "value": "review"},
        {"click": "[data-project-settings-save]"},
        {"click": "[data-confirm]"},
        {"click": "[data-settings-reload]"},
    ]);
    let responses = json!([project_settings("merge_local", 3), projects(), machine_settings(true), options(), stale, project_settings("push", 5)]);
    let Some(out) = run(responses, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    let refused = &frames[frames.len() - 2];
    assert!(shown(refused).iter().any(|t| t == "The project's settings changed since they were read"), "{out}");
    assert_eq!(count(refused, "[data-settings-reload]"), 1, "{out}");
    assert_eq!(count(refused, "[data-delivery]"), 1, "the edit is kept: {out}");
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Push only"), "Reload shows the settings as they are now: {out}");
    assert_eq!(count(frame, "[data-settings-reload]"), 0, "{out}");
    assert_eq!(count(frame, "[data-delivery]"), 0, "{out}");
}

#[test]
fn with_remote_actions_off_edit_says_so_and_offers_nothing_to_send() {
    let steps = json!([{"click": "[data-project-settings-edit]"}]);
    let Some(out) = run(json!([project_settings("review", 1), projects(), machine_settings(false)]), steps) else { return };
    let frame = last(&out);
    assert!(shown(frame).iter().any(|t| t == "Turn remote actions on in Settings to change this project's settings."), "{out}");
    assert_eq!(count(frame, "[data-role-harness]"), 0, "{out}");
    assert!(!out["requests"].as_array().unwrap().iter().any(|r| r["url"] == "/api/settings/options"), "the options are not read: {out}");
    assert!(posts(&out).is_empty(), "{out}");
}

#[test]
fn cancel_closes_the_editor_and_sends_nothing() {
    let steps = json!([
        {"click": "[data-project-settings-edit]"},
        {"click": "[data-role-reset=\"implementer\"]"},
        {"click": "[data-cancel]"},
    ]);
    let Some(out) = run(json!([project_settings("review", 1), projects(), machine_settings(true), options()]), steps) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-role-harness]"), 0, "{out}");
    assert!(shown(frame).iter().any(|t| t == "claude sonnet, effort medium"), "the project's own agent is shown again: {out}");
    assert!(posts(&out).is_empty(), "{out}");
}

#[test]
fn edit_keeps_a_project_agent_whose_harness_or_model_is_not_offered_so_saving_the_delivery_leaves_it_as_it_was() {
    let unoffered = json!({"status": 200, "json": {"settings": {
        "implementer": {"project": {"harness": "claude", "model": "haiku", "effort": "medium"}, "fallback": null},
        "reviewer": {"project": {"harness": "codex", "model": "gpt", "effort": null}, "fallback": null},
        "delivery": "review",
        "revision": 3
    }}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = json!([
        {"click": "[data-project-settings-edit]"},
        {"set": "[data-delivery]", "value": "push"},
        {"click": "[data-project-settings-save]"},
        {"click": "[data-confirm]"},
    ]);
    let Some(out) = run(json!([unoffered, projects(), machine_settings(true), options(), ok, project_settings("push", 4)]), steps) else { return };
    let editing = &out["frames"].as_array().unwrap()[2];
    assert_eq!(count(editing, "[data-role-model]"), 2, "both roles keep their own agent: {out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(
        body(&sent[0]),
        json!({"implementer": {"harness": "claude", "model": "haiku", "effort": "medium"}, "reviewer": {"harness": "codex", "model": "gpt", "effort": null}, "delivery": "push", "revision": 3})
    );
}
