//! A run blocked because main has uncommitted changes to files it changes says "Waiting for you" on its page and
//! in the runs list, names those files (as text, ten at most), and offers Resume and "Set aside my changes and
//! merge", which asks first and posts to the merge-set-aside route. A set-aside merge's conflict note, with the
//! stash it left the changes in, shows on the run's timeline like a warning.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 6] = ["[data-dirty-checkout]", "[data-merge-set-aside]", "[data-resume]", "[data-confirm]", "[data-set-aside]", "[data-set-aside-conflict]"];
const QUESTION: &str = "Your changes to these files are stashed, the run is merged, and your changes are put back. If they conflict, they stay in the stash and orq tells you where.";

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
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn one(id: &str, state: &str, dirty: Option<Vec<String>>) -> Value {
    let mut row = json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    if let Some(files) = dirty {
        row["dirty_files"] = json!(files);
    }
    row
}

fn detail(shown: Value, events: Vec<Value>, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let id = shown["run_id"].as_str().unwrap().to_string();
    let body = json!({"status": 200, "json": {"run": shown, "events": events, "next_cursor": null, "remote_actions": true}});
    let mut responses = vec![body, projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn event(id: &str, kind: &str, extra: Value) -> Value {
    let mut event = json!({"event_id": id, "kind": kind, "stage": null, "attempt": 1, "occurred_at": "2026-11-01T10:00:00Z"});
    for (key, value) in extra.as_object().unwrap() {
        event[key] = value.clone();
    }
    event
}

#[test]
fn a_run_waiting_on_the_checkout_names_its_files_as_text_and_merges_setting_them_aside_after_asking() {
    let hostile = "src/<b>evil</b>.rs";
    let mut files: Vec<String> = vec![hostile.to_string()];
    files.extend((1..=11).map(|n| format!("src/file{n}.rs")));
    let ok = json!({"status": 202, "json": {"run": "run-2"}});
    let Some(out) = detail(one("r-dirty", "blocked", Some(files)), vec![], vec![ok], vec![json!({"click": "[data-merge-set-aside]"}), json!({"click": "[data-confirm]"})]) else { return };

    let first = &out["frames"][1];
    assert_eq!(count(first, "[data-dirty-checkout]"), 1, "{out}");
    let shown = texts(first);
    assert!(shown.iter().any(|t| t == "Waiting for you"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "main has uncommitted changes to files this run changes"), "{shown:?}");
    assert!(shown.iter().any(|t| t == hostile), "the hostile name is text: {shown:?}");
    assert!(shown.iter().any(|t| t == "src/file9.rs"), "{shown:?}");
    assert!(!shown.iter().any(|t| t == "src/file10.rs"), "ten files at most: {shown:?}");
    assert!(shown.iter().any(|t| t == "and 2 more"), "{shown:?}");
    assert_eq!(first["settled"]["injected"], json!(0), "{out}");
    assert_eq!(count(first, "[data-resume]"), 1, "Resume is offered too: {out}");
    assert!(texts(first).iter().any(|t| t == "Set aside my changes and merge"), "{out}");

    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == QUESTION), "{asked:?}");
    let posts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-dirty/merge-set-aside");
    assert_eq!(posts[0]["body"], "{}");
    assert!(texts(last(&out)).iter().any(|t| t == "Merging as run run-2."), "{out}");
}

#[test]
fn only_a_blocked_run_naming_files_offers_the_set_aside_merge_and_the_list_says_it_is_waiting() {
    let Some(out) = detail(one("r-moved", "blocked", None), vec![], vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-dirty-checkout]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-merge-set-aside]"), 0, "{out}");
    let Some(out) = detail(one("r-run", "running", Some(vec!["src/lib.rs".into()])), vec![], vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-merge-set-aside]"), 0, "{out}");

    let rows = vec![one("r-dirty", "blocked", Some(vec!["src/<i>x</i>.rs".into()])), one("r-moved", "blocked", None)];
    let list = json!({"status": 200, "json": {"runs": rows, "next_cursor": null, "total": 2, "running": 0, "remote_actions": true}});
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations", "responses": [list, projects()], "steps": [{"submit": TOKEN}], "selectors": SELECTORS})) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-dirty-checkout]"), 1, "{out}");
    assert_eq!(count(frame, "[data-merge-set-aside]"), 1, "{out}");
    assert!(texts(frame).iter().any(|t| t == "Waiting for you"), "{out}");
    assert!(texts(frame).iter().any(|t| t == "src/<i>x</i>.rs"), "{out}");
    assert_eq!(frame["settled"]["injected"], json!(0), "{out}");
}

#[test]
fn a_set_aside_merge_that_conflicted_says_which_stash_holds_the_changes_like_a_warning() {
    let hostile = "src/<b>evil</b>.rs";
    let note = format!("your changes to {hostile} conflicted with this run and are in stash 0123abcd (orq set-aside r-1)");
    let conflicted = event("e-2", "merge.set_aside", json!({"set_aside": {"stash": "0123abcd", "message": "orq set-aside r-1", "restored": false, "files": [hostile], "conflicted": [hostile], "note": note}}));
    let restored = event("e-3", "merge.set_aside", json!({"set_aside": {"stash": "4567ef01", "message": "orq set-aside r-1", "restored": true, "files": ["src/lib.rs"], "conflicted": []}}));
    let Some(out) = detail(one("r-1", "completed", None), vec![event("e-1", "run.started", json!({})), conflicted, restored], vec![], vec![]) else { return };
    let frame = last(&out);
    assert!(count(frame, "[data-set-aside]") >= 2, "{out}");
    assert!(count(frame, "[data-set-aside-conflict]") >= 1, "{out}");
    let shown = texts(frame);
    assert!(shown.iter().any(|t| t == "Your changes are in stash 0123abcd (orq set-aside r-1)"), "{shown:?}");
    assert!(shown.iter().any(|t| t == hostile), "the conflicted file is text: {shown:?}");
    assert!(shown.iter().any(|t| t == &note), "{shown:?}");
    assert!(shown.iter().any(|t| t == "Your changes were set aside and put back."), "{shown:?}");
    assert!(frame["settled"]["classes"].as_array().unwrap().contains(&json!("set-aside-conflict")), "{out}");
    assert_eq!(frame["settled"]["injected"], json!(0), "{out}");
}
