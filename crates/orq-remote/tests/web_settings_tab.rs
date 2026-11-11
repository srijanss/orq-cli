//! The global Settings page: the run settings from `/api/settings`, drawn as text.
//! Signed in with the token and with no settings version, nothing on it can change a setting.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    run_scenario(json!({"url": url, "responses": responses, "steps": steps}))
}

fn run_scenario(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings() -> Value {
    json!({"status": 200, "json": {"settings": {
        "implementer": {"harness": "claude", "model": "opus", "effort": "high"},
        "reviewer": {"harness": "pi", "model": "<b>gpt</b>", "effort": null},
        "models": [{"name": "opus", "harness": "claude", "model": "opus", "effort": null}]
    }}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_settings_page_reads_the_global_endpoint_once_and_lists_the_implementer_and_reviewer_only() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["requests"][0]["url"], "/api/settings", "{out}");
    assert_eq!(out["requests"][0]["method"], "GET", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let t = shown(&out["frames"][1]);
    let at = |needle: &str| t.iter().position(|x| x == needle).unwrap_or_else(|| panic!("{needle} missing: {t:?}"));
    assert!(at("Settings") < at("Implementer") && at("Implementer") < at("Reviewer") && !t.iter().any(|x| x == "Models"), "{t:?}");
    for text in ["claude", "opus", "high", "pi", "<b>gpt</b>"] {
        assert!(t.iter().any(|x| x == text), "{text} missing: {t:?}");
    }
    assert!(!t.iter().any(|x| x.contains("not available yet")), "{t:?}");
}

#[test]
fn a_setting_with_no_value_shows_a_dash_and_says_it_is_not_set() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    let t = shown(&out["frames"][1]);
    assert!(t.iter().any(|x| x == "—"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("not set")), "{t:?}");
    let Some(out) = run("/settings", json!([{"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": []}}}]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(shown(&out["frames"][1]).iter().filter(|x| x.contains("not set")).count(), 6, "every field of both agents says so: {out}");
}

#[test]
fn without_a_settings_version_nothing_on_the_page_can_change_a_setting() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1]["settled"];
    assert_eq!(frame["inputs"], json!([]), "{out}");
    assert_eq!(frame["buttons"], json!(["Light", "System", "Dark", "Sign out"]), "only this browser's theme and signing out: {out}");
    assert_eq!(frame["links"].as_array().unwrap().iter().map(|l| l["href"].clone()).collect::<Vec<_>>(), [json!("/settings/audit")], "only the read-only audit link: {out}");
}

#[test]
fn unusable_settings_draw_nothing_and_a_refused_token_asks_again() {
    let Some(out) = run("/settings", json!([{"status": 200, "json": {"settings": "x"}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(!shown(&out["frames"][1]).iter().any(|x| x == "Implementer"), "{out}");
    let Some(out) = run("/settings", json!([{"status": 401}]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
}

#[test]
fn an_address_under_settings_is_not_found_and_never_shows_the_inbox() {
    for odd in ["/settings/", "/settings/extra"] {
        let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}, {"go": odd}])) else { return };
        assert_eq!(out["requests"].as_array().unwrap().len(), 1, "{odd}: only the settings were asked for: {out}");
        let t = shown(&out["frames"][2]);
        assert!(t.iter().any(|x| x == "Not found") && !t.iter().any(|x| x == "Implementer"), "{odd}: {t:?}");
    }
}

#[test]
fn the_page_says_whether_remote_actions_are_on_and_has_no_text_field() {
    for (value, word) in [(false, "Off"), (true, "On")] {
        let body = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": value}}});
        let scenario = json!({"url": "/settings", "responses": [body], "steps": [{"submit": TOKEN}], "selectors": [format!("[data-remote-actions={}]", word.to_lowercase())]});
        let Some(out) = run_scenario(scenario) else { return };
        let lines = shown(&out["frames"][1]);
        assert!(lines.iter().any(|l| l == "Remote actions") && lines.iter().any(|l| l == word), "{word}: {lines:?}");
        assert_eq!(out["frames"][1]["settled"]["matches"][format!("[data-remote-actions={}]", word.to_lowercase())], 1, "the word sits in the Remote actions row: {out}");
        assert!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().is_empty(), "{out}");
    }
}

#[test]
fn the_theme_can_be_chosen_on_the_page_and_the_sidebar_pill_follows() {
    let sel = json!(["[data-theme-choice=dark][aria-pressed=true]", "[data-theme-toggle] .theme-option[data-active][data-mode=dark]"]);
    let steps = json!([{"submit": TOKEN}, {"click": "[data-theme-choice=dark]", "index": 0}, {"click": "[data-theme-choice=system]", "index": 0}]);
    let Some(out) = run_scenario(json!({"url": "/settings", "responses": [settings()], "steps": steps, "selectors": sel})) else { return };
    let dark = &out["frames"][2]["settled"];
    assert_eq!(dark["theme"], "dark", "{out}");
    assert_eq!(dark["localTheme"], "dark", "{out}");
    assert_eq!(dark["matches"]["[data-theme-choice=dark][aria-pressed=true]"], 1, "{out}");
    assert_eq!(dark["matches"]["[data-theme-toggle] .theme-option[data-active][data-mode=dark]"], 1, "the sidebar pill shows the same choice: {out}");
    assert_eq!(dark["toggles"], 1, "still one sidebar pill: {out}");
    let system = &out["frames"][3]["settled"];
    assert_eq!(system["theme"], "", "{out}");
    assert!(system["localTheme"].is_null(), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "choosing a theme sends nothing: {out}");
}

#[test]
fn signing_out_from_the_page_returns_to_the_sign_in() {
    let steps = json!([{"submit": TOKEN}, {"click": "[data-sign-out]", "index": 0}]);
    let Some(out) = run("/settings", json!([settings()]), steps) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(!last["settled"]["shell"].as_bool().unwrap(), "{out}");
    assert_eq!(last["settled"]["inputs"].as_array().unwrap().len(), 1, "the token box is back: {out}");
}
