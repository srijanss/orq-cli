//! With remote actions on, Settings has a Storage card with "Preview cleanup": it reads what a cleanup would
//! remove and lists it with its totals, then "Clean up" asks with those totals and sends the preview's hash.
//! A set that changed since the preview, or a busy prune, is shown in the server's words, and nothing is sent
//! until the question is answered.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 5] = ["[data-storage-preview]", "[data-storage-prune]", "[data-storage-line]", "[data-confirm]", "[data-cancel]"];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings(on: bool) -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

fn preview(files: u64, metadata: u64) -> Value {
    let lines = if files + metadata == 0 { vec!["Nothing to prune.".to_string()] } else { vec!["3 file(s), 2.5 KiB across 2 run(s):".to_string(), "  runs/r-1/a.log  1.0 KiB  past its retention".to_string()] };
    json!({"status": 200, "json": {"lines": lines, "files": files, "bytes": 2560, "size": "2.5 KiB", "runs": 2, "metadata": metadata, "hash": "h1"}})
}

fn as_token(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/settings", "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn the_cleanup_is_offered_only_with_actions_on() {
    let Some(out) = as_token(vec![settings(false)], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-storage-preview]"), 0, "{out}");
    let Some(out) = as_token(vec![settings(true)], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-storage-preview]"), 1, "{out}");
    assert_eq!(count(last(&out), "[data-storage-prune]"), 0, "Clean up comes only after a preview: {out}");
}

#[test]
fn preview_lists_the_set_and_clean_up_asks_with_its_totals_then_sends_its_hash() {
    let pruned = json!({"status": 200, "json": {"lines": ["Removed 3 file(s), freed 2.5 KiB."]}});
    let steps = vec![json!({"click": "[data-storage-preview]"}), json!({"click": "[data-storage-prune]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = as_token(vec![settings(true), preview(3, 1), pruned], steps) else { return };
    let read = out["requests"].as_array().unwrap().iter().find(|r| r["url"] == "/api/settings/storage/preview").cloned().expect("the preview is read");
    assert_eq!(read["method"], "GET");
    assert_eq!(read["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let previewed = &out["frames"][2];
    assert_eq!(count(previewed, "[data-storage-line]"), 2, "{out}");
    assert!(shown(previewed).iter().any(|t| t == "3 file(s), 2.5 KiB across 2 run(s):"), "{out}");
    let asked = &out["frames"][3];
    assert!(shown(asked).iter().any(|t| t == "Clean up 3 file(s), 2.5 KiB across 2 run(s), and the metadata of 1 run(s)?"), "{out}");
    assert!(shown(asked).iter().any(|t| t == "Yes, clean up"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/settings/storage/prune");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(serde_json::from_str::<Value>(sent[0]["body"].as_str().unwrap()).unwrap(), json!({"hash": "h1"}));
    let done = last(&out);
    assert!(shown(done).iter().any(|t| t == "Removed 3 file(s), freed 2.5 KiB."), "{out}");
    assert_eq!(count(done, "[data-storage-prune]"), 0, "the preview is used up: {out}");
    assert_eq!(count(done, "[data-storage-preview]"), 1, "{out}");
}

#[test]
fn nothing_to_prune_offers_no_clean_up() {
    let Some(out) = as_token(vec![settings(true), preview(0, 0)], vec![json!({"click": "[data-storage-preview]"})]) else { return };
    assert!(shown(last(&out)).iter().any(|t| t == "Nothing to prune."), "{out}");
    assert_eq!(count(last(&out), "[data-storage-prune]"), 0, "{out}");
}

#[test]
fn a_changed_set_is_shown_in_the_servers_words_and_cancel_sends_nothing() {
    let changed = json!({"status": 409, "json": {"error": "The cleanup changed since the preview; nothing was removed. Preview it again."}});
    let steps = vec![json!({"click": "[data-storage-preview]"}), json!({"click": "[data-storage-prune]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = as_token(vec![settings(true), preview(3, 0), changed], steps) else { return };
    assert!(shown(last(&out)).iter().any(|t| t == "The cleanup changed since the preview; nothing was removed. Preview it again."), "{out}");
    assert_eq!(count(last(&out), "[data-storage-prune]"), 0, "a refused preview is not offered again: {out}");
    assert_eq!(count(last(&out), "[data-storage-preview]"), 1, "{out}");

    let cancel = vec![json!({"click": "[data-storage-preview]"}), json!({"click": "[data-storage-prune]"}), json!({"click": "[data-cancel]"})];
    let Some(out) = as_token(vec![settings(true), preview(3, 0)], cancel) else { return };
    assert!(posts(&out).is_empty(), "{out}");
    assert_eq!(count(last(&out), "[data-storage-prune]"), 1, "Cancel goes back to the preview: {out}");

    let failed = json!({"status": 500, "json": {"error": "the cleanup could not be previewed"}});
    let Some(out) = as_token(vec![settings(true), failed], vec![json!({"click": "[data-storage-preview]"})]) else { return };
    assert!(shown(last(&out)).iter().any(|t| t == "the cleanup could not be previewed"), "{out}");
    assert_eq!(count(last(&out), "[data-storage-prune]"), 0, "{out}");
}
