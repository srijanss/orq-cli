//! A feature's kind on the web: a Frontend badge on frontend rows of the Features and Orchestrations pages, a
//! filter All / Frontend / Standard kept in the URL (`?kind=frontend`), one select on a phone, and on a
//! feature's page a Frontend switch (an authenticated action) and where its design package is expected.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;
use web_chromium::{get, measure, Reply};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = ["[data-frontend-badge]", "[data-feature-kind]", "[data-run-kind]", "[data-frontend-switch]", "[data-design-package]", "[data-feature-row]", ON];
const ON: &str = "[data-frontend-switch]:checked";

fn on(frame: &Value) -> bool {
    count(frame, ON) == 1
}

fn run(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn feature(name: &str, kind: Option<&str>) -> Value {
    let mut f = json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    if let Some(kind) = kind {
        f["kind"] = json!(kind);
    }
    f
}

/// "page" is frontend with its own design package, "engine" standard, and "older" comes from a server that says
/// no kind (standard).
fn ledger(actions: bool) -> Value {
    let mut page = feature("page", Some("frontend"));
    page["design_package"] = json!("page");
    json!({"status": 200, "json": {"features": [page, feature("engine", Some("standard")), feature("older", None)], "remote_actions": actions}})
}

fn run_row(id: &str, feature: &str, kind: Option<&str>) -> Value {
    let mut r = json!({"run_id": id, "feature": feature, "target_branch": "main", "state": "running", "stage": null, "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"});
    if let Some(kind) = kind {
        r["feature_kind"] = json!(kind);
    }
    r
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [run_row("run-p", "page", Some("frontend")), run_row("run-e", "engine", Some("standard")), run_row("run-o", "older", None)]}})
}

fn no_runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
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

fn shows(frame: &Value, name: &str) -> bool {
    texts(frame).iter().any(|t| t == name)
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

#[test]
fn the_features_page_badges_frontend_rows_only_and_filters_by_kind_in_the_url() {
    let Some(out) = run("/projects/alpha/features", vec![ledger(false), projects()], vec![]) else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-frontend-badge]"), 1, "only the frontend row: {out}");
    assert_eq!(count(end, "[data-feature-kind]"), 1, "{out}");
    assert!(shows(end, "Frontend"), "{out}");
    assert!(shows(end, "page") && shows(end, "engine") && shows(end, "older"), "All shows every row: {out}");

    let pick = |kind: &str| vec![json!({"set": "[data-feature-kind]", "value": kind})];
    let Some(out) = run("/projects/alpha/features", vec![ledger(false), projects()], pick("frontend")) else { return };
    let end = last(&out);
    assert!(shows(end, "page") && !shows(end, "engine") && !shows(end, "older"), "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features?kind=frontend", "{out}");

    let Some(out) = run("/projects/alpha/features", vec![ledger(false), projects()], pick("standard")) else { return };
    let end = last(&out);
    assert!(!shows(end, "page") && shows(end, "engine") && shows(end, "older"), "a feature with no kind is standard: {out}");
    assert_eq!(count(end, "[data-frontend-badge]"), 0, "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features?kind=standard", "{out}");

    let mut back = pick("frontend");
    back.extend(pick("all"));
    let Some(out) = run("/projects/alpha/features", vec![ledger(false), projects()], back) else { return };
    let end = last(&out);
    assert!(shows(end, "page") && shows(end, "engine") && shows(end, "older"), "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features", "All leaves no kind in the URL: {out}");
}

#[test]
fn the_features_filter_survives_a_reload() {
    let Some(out) = run("/projects/alpha/features?kind=frontend", vec![ledger(false), projects()], vec![]) else { return };
    let end = last(&out);
    assert!(shows(end, "page") && !shows(end, "engine") && !shows(end, "older"), "{out}");
    let Some(out) = run("/projects/alpha/features?kind=standard", vec![ledger(false), projects()], vec![]) else { return };
    let end = last(&out);
    assert!(!shows(end, "page") && shows(end, "engine") && shows(end, "older"), "{out}");
    let Some(out) = run("/projects/alpha/features?kind=nonsense", vec![ledger(false), projects()], vec![]) else { return };
    let end = last(&out);
    assert!(shows(end, "page") && shows(end, "engine") && shows(end, "older"), "an unknown kind shows all: {out}");
}

fn run_names(frame: &Value) -> Vec<String> {
    texts(frame).into_iter().filter(|t| ["page", "engine", "older"].contains(&t.as_str())).collect()
}

#[test]
fn the_orchestrations_page_badges_runs_of_frontend_features_and_filters_by_kind() {
    let Some(out) = run("/projects/alpha/orchestrations", vec![runs(), projects()], vec![]) else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-frontend-badge]"), 1, "{out}");
    assert_eq!(count(end, "[data-run-kind]"), 1, "{out}");
    assert_eq!(run_names(end), ["page", "engine", "older"], "{out}");

    let Some(out) = run("/projects/alpha/orchestrations", vec![runs(), projects()], vec![json!({"set": "[data-run-kind]", "value": "frontend"})]) else { return };
    let end = last(&out);
    assert_eq!(run_names(end), ["page"], "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/orchestrations?kind=frontend", "{out}");

    let Some(out) = run("/projects/alpha/orchestrations", vec![runs(), projects()], vec![json!({"set": "[data-run-kind]", "value": "standard"})]) else { return };
    let end = last(&out);
    assert_eq!(run_names(end), ["engine", "older"], "{out}");
    assert_eq!(count(end, "[data-frontend-badge]"), 0, "{out}");

    let Some(out) = run("/projects/alpha/orchestrations?kind=frontend", vec![runs(), projects()], vec![]) else { return };
    assert_eq!(run_names(last(&out)), ["page"], "the filter survives a reload: {out}");
}

fn detail(feature: &str, actions: bool, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut responses = vec![ledger(actions), no_runs(), projects()];
    responses.extend(extra);
    run(&format!("/projects/alpha/features/{feature}"), responses, steps)
}

#[test]
fn a_feature_page_says_where_its_design_package_is_expected_and_offers_the_switch_only_with_actions_on() {
    let Some(out) = detail("page", false, vec![], vec![]) else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-frontend-switch]"), 0, "{out}");
    assert_eq!(count(end, "[data-design-package]"), 1, "{out}");
    assert!(texts(end).iter().any(|t| t == "designs/page/"), "{out}");

    let Some(out) = detail("page", true, vec![], vec![]) else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-frontend-switch]"), 1, "{out}");
    assert!(on(end), "{out}");
}

/// "home" is in the shared package egiftit-homepage, "lone" is frontend with no package on disk, "engine" standard.
fn package_ledger() -> Value {
    let mut home = feature("home", Some("frontend"));
    home["design_package"] = json!("egiftit-homepage");
    let mut lone = feature("lone", Some("frontend"));
    lone["design_package"] = Value::Null;
    json!({"status": 200, "json": {"features": [home, lone, feature("engine", Some("standard"))], "remote_actions": false}})
}

#[test]
fn a_feature_page_names_the_shared_package_folder_its_feature_reads_or_says_none_was_found() {
    let page = |name: &str| run(&format!("/projects/alpha/features/{name}"), vec![package_ledger(), no_runs(), projects()], vec![]);
    let Some(out) = page("home") else { return };
    let end = last(&out);
    assert_eq!(count(end, "[data-design-package]"), 1, "{out}");
    assert!(shows(end, "designs/egiftit-homepage/"), "{out}");
    assert!(!shows(end, "designs/home/"), "never the feature's own name: {out}");

    let Some(out) = page("lone") else { return };
    let end = last(&out);
    assert!(shows(end, "No design package found"), "{out}");
    assert!(!shows(end, "designs/lone/"), "{out}");

    let Some(out) = page("engine") else { return };
    let end = last(&out);
    assert!(!shows(end, "No design package found"), "a standard feature needs none: {out}");
}

#[test]
fn the_switch_posts_the_kind_and_a_refusal_shows_the_reason() {
    let ok = json!({"status": 200, "json": {"feature": "engine", "kind": "frontend"}});
    let after = json!({"status": 200, "json": {"features": [feature("page", Some("frontend")), feature("engine", Some("frontend")), feature("older", None)], "remote_actions": true}});
    let Some(out) = detail("engine", true, vec![ok, after, no_runs()], vec![json!({"click": "[data-frontend-switch]"})]) else { return };
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/engine/kind");
    assert_eq!(sent[0]["body"], json!({"kind": "frontend"}).to_string());
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert!(on(last(&out)), "{out}");

    let refused = json!({"status": 409, "json": {"error": "engine is not in this project's ledger"}});
    let Some(out) = detail("engine", true, vec![refused], vec![json!({"click": "[data-frontend-switch]"})]) else { return };
    assert!(texts(last(&out)).iter().any(|t| t == "engine is not in this project's ledger"), "{out}");
    assert!(!on(last(&out)), "{out}");
    assert_eq!(count(last(&out), "[data-frontend-switch]"), 1, "{out}");

    let Some(out) = detail("page", true, vec![json!({"status": 200, "json": {"feature": "page", "kind": "standard"}}), ledger(true), no_runs()], vec![json!({"click": "[data-frontend-switch]"})]) else { return };
    assert_eq!(posts(&out)[0]["body"], json!({"kind": "standard"}).to_string(), "{out}");
}

#[test]
fn the_switch_is_refused_without_auth() {
    let unauthorized = json!({"status": 401, "json": {"error": "unauthorized"}});
    let Some(out) = detail("engine", true, vec![unauthorized], vec![json!({"click": "[data-frontend-switch]"})]) else { return };
    let end = last(&out);
    assert_eq!(posts(&out).len(), 1, "{out}");
    assert_eq!(count(end, "[data-frontend-switch]"), 0, "a 401 signs out: {out}");
    assert!(texts(end).iter().any(|t| t.starts_with("Sign in with token")), "the sign-in form is back: {out}");
}

const PHONE: u32 = 390;

fn layout_replies() -> Vec<Reply> {
    let features = json!({"features": [feature("page", Some("frontend")), feature("engine", Some("standard"))], "remote_actions": true});
    let runs = json!({"runs": [run_row("run-p", "page", Some("frontend")), run_row("run-e", "engine", Some("standard"))], "next_cursor": null, "total": 2, "running": 2});
    vec![
        get("/api/projects", json!({"projects": [{"id": "demo", "name": "demo", "available": true, "features": 2, "active_runs": 2, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]})),
        get("/api/projects/demo/features", features),        get("/api/projects/demo/runs*", runs),
    ]
}

#[test]
fn at_390_wide_each_filter_is_one_select_and_nothing_scrolls_sideways() {
    let Some(chromium) = web_chromium::binary() else { return };
    for (url, select) in [("/projects/demo/features?kind=frontend", "[data-feature-kind]"), ("/projects/demo/orchestrations?kind=frontend", "[data-run-kind]")] {
        let steps = json!([{"wait": "[data-frontend-badge]"}, {"boxes": select}, {"boxes": "[role=tab][data-kind]"}, {"styles": select, "props": ["display"]}]);
        let out = measure(&chromium, PHONE, url, &layout_replies(), steps.clone());
        let result = &out.result;
        assert!(result.get("error").is_none(), "{url}: {result}\nunanswered: {:?}", out.unanswered);
        assert_eq!(result["boxes"][select].as_array().map(Vec::len), Some(1), "{url}: one select: {result}");
        assert_eq!(result["boxes"]["[role=tab][data-kind]"].as_array().map(Vec::len), Some(0), "{url}: no tabs: {result}");
        assert_ne!(result["styles"][select][0]["display"], "none", "{url}: {result}");
        assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{url}: the page scrolls sideways: {result}");
        assert_eq!(result["offenders"], json!([]), "{url}: text runs past the screen: {result}");
    }
}
