//! Landmarks, polite announcements that leave log lines out, reduced motion, visible focus and
//! 44px tap targets.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

fn css() -> String {
    std::fs::read_to_string(format!("{}/web/app.css", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn a_signed_in_page_has_one_banner_one_labelled_nav_and_one_main() {
    let ledger = json!({"status": 200, "json": {"features": []}});
    let Some(out) = run("/projects/alpha/features", json!([ledger, projects()]), json!([{"submit": TOKEN}])) else { return };
    let a = &out["frames"][1]["settled"]["a11y"];
    assert_eq!(a["mains"], 1, "{out}");
    assert!(a["banners"].as_i64().unwrap() <= 1, "a page header inside main is not a second banner: {out}");
    assert!(a["navs"].as_array().unwrap().iter().all(|n| n.as_str().is_some_and(|s| !s.is_empty())), "every nav is labelled: {out}");
}

#[test]
fn the_sign_in_page_has_one_main() {
    let Some(out) = run("/", json!([]), json!([])) else { return };
    assert_eq!(out["frames"][0]["settled"]["a11y"]["mains"], 1, "{out}");
}

#[test]
fn a_page_change_is_announced_politely_and_log_lines_are_never_in_a_live_region() {
    let ledger = json!({"status": 200, "json": {"features": []}});
    let Some(out) = run("/projects/alpha/features", json!([ledger, projects()]), json!([{"submit": TOKEN}])) else { return };
    let live = out["frames"][1]["settled"]["a11y"]["live"].as_array().unwrap();
    assert!(live.iter().any(|l| l["live"] == "polite" && l["text"].as_str().unwrap().contains("Features")), "{out}");

    let list = json!({"status": 200, "json": {"files": [{"name": "implement.log", "bytes": 9}]}});
    let page = json!({"status": 200, "json": {"name": "implement.log", "lines": ["secret-log-line-xyz"], "next_cursor": null, "truncated": false}});
    let run_page = json!({"status": 200, "json": {"run": {"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "failed", "stage": null, "started_at": null, "updated_at": null}, "events": [], "next_cursor": null}});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run("/projects/alpha/orchestrations/run-1/logs/implement.log", json!([run_page, projects(), stream, list, page]), json!([{"submit": TOKEN}])) else { return };
    let settled = &out["frames"][1]["settled"];
    assert!(settled["texts"].as_array().unwrap().iter().any(|t| t.as_str().is_some_and(|t| t.contains("secret-log-line-xyz"))), "{out}");
    for region in settled["a11y"]["live"].as_array().unwrap() {
        assert!(!region["text"].as_str().unwrap().contains("secret-log-line-xyz"), "a log line is announced: {out}");
    }
}

#[test]
fn motion_is_reduced_on_request_focus_is_visible_and_targets_are_44px() {
    let css = css();
    let reduced = css.split("@media (prefers-reduced-motion: reduce)").nth(1).expect("a reduced-motion rule");
    let reduced = reduced.split("\n}").next().unwrap();
    assert!(reduced.contains("animation: none") && reduced.contains("transition: none"), "it stops animation and transitions: {reduced}");
    let focus = css.split(":focus-visible").nth(1).expect("a :focus-visible rule");
    assert!(focus.split('}').next().unwrap().contains("outline: 3px"), "focus is drawn");
    for control in ["input", "button", "select", "a"] {
        let sized = css.lines().any(|l| l.starts_with(&format!("{control} {{")) && l.contains("min-height: 44px"));
        assert!(sized, "{control} has a rule with a 44px minimum height");
    }
}

fn announced(out: &Value, frame: usize) -> String {
    out["frames"][frame]["settled"]["a11y"]["live"].as_array().unwrap().iter().filter(|l| l["live"] == "polite").map(|l| l["text"].as_str().unwrap().to_string()).collect::<Vec<_>>().join(" ")
}

#[test]
fn moving_to_another_page_changes_the_text_of_the_one_polite_region() {
    let ledger = json!({"status": 200, "json": {"features": []}});
    let runs = json!({"status": 200, "json": {"runs": []}});
    let steps = json!([{"submit": TOKEN}, {"nav": "nav a[href$='/orchestrations']"}]);
    let Some(out) = run("/projects/alpha/features", json!([ledger, projects(), runs]), steps) else { return };
    let last = out["frames"].as_array().unwrap().len() - 1;
    assert!(announced(&out, 1).contains("Features"), "{out}");
    assert!(announced(&out, last).contains("Orchestrations") && !announced(&out, last).contains("Features"), "the same region now names the new page: {out}");
}

#[test]
fn a_run_page_is_announced_by_its_run_and_project() {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": null, "started_at": null, "updated_at": null});
    let timeline = json!({"status": 200, "json": {"run": header, "events": [], "next_cursor": null}});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run("/projects/alpha/orchestrations/run-1", json!([timeline, projects(), stream]), json!([{"submit": TOKEN}])) else { return };
    let text = announced(&out, 1);
    assert!(text.contains("run-1") && text.contains("alpha"), "{text}: {out}");
}

#[test]
fn a_report_opened_at_its_short_address_is_announced_by_its_run() {
    let detail = json!({"status": 200, "json": {
        "run_id": "run-1", "reported_head_sha": "abc1234", "observed_head_sha": "abc1234", "stale": false, "freshness": "current", "link_allowed": false, "link": null,
        "reviewed_head_sha": "1111111aaaa", "provider_head_sha": "2222222bbbb", "checked_at": "2026-11-09T10:00:00Z",
        "summary": ["ok"], "risks": [], "files": [], "tests": [], "rounds": []
    }});
    let Some(out) = run("/reviews/run-1", json!([detail]), json!([{"submit": TOKEN}])) else { return };
    let text = announced(&out, 1);
    assert!(text.contains("Review") && text.contains("run-1"), "{text}: {out}");
}
