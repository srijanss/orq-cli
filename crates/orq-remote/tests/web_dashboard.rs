//! The Dashboard: one card per project with its counts; choosing a card opens the project.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(responses: Value, steps: Value) -> Option<Value> {
    run_at("/dashboard", responses, steps)
}

fn run_at(url: &str, responses: Value, steps: Value) -> Option<Value> {
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
    json!({"status": 200, "json": {"projects": [
        {"id": "alpha", "name": "alpha", "available": true, "features": 3, "active_runs": 2, "reviews_waiting": 1, "failed_runs": 4, "failed_window_hours": 24},
        {"id": "broken", "name": "broken", "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": 24}
    ]}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn the_dashboard_asks_for_the_projects_with_the_held_token_and_shows_one_card_each_with_its_counts() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"][0]["url"], "/api/projects", "{out}");
    assert_eq!(out["requests"][0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let frame = &out["frames"][1];
    assert_eq!(frame["settled"]["path"], "/dashboard");
    assert_eq!(frame["settled"]["cards"], 2, "{out}");
    let text = shown(frame);
    for expected in ["Features", "Active runs", "Last updated"] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(!text.contains("Waiting |") && !text.contains("Failed (24h)"), "a card shows only three figures: {text}");
}

#[test]
fn a_project_that_cannot_be_read_is_listed_as_unavailable_without_counts() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("Unavailable"), "{text}");
    assert_eq!(text.matches("Features").count(), 1, "only the readable project has counts: {text}");
}

#[test]
fn choosing_a_project_opens_it() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}, {"click": "button", "index": 0}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/features", "{out}");
}

#[test]
fn no_projects_says_so() {
    let Some(out) = run(json!([{"status": 200, "json": {"projects": []}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).contains("No projects found."), "{out}");
}

fn inbox() -> Value {
    json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "first-feature", "status": "approved", "stale": false}]}})
}

// The other page's data is not this page's: while waiting there is a skeleton and no cards, and
// after a failure there is nothing marked Outdated.
fn assert_ownership(frame: &Value, held: &Value, text: &str) {
    assert_eq!(frame["settled"]["cards"], 0, "{held}: another page's data drawn as cards: {text}");
    assert!(!text.contains("Outdated"), "{held}: {text}");
    if held.get("hold").is_some() {
        assert_eq!(frame["settled"]["skeletons"], 1, "{held}: {text}");
    }
}

#[test]
fn moving_between_the_inbox_and_the_dashboard_never_shows_one_kind_of_data_as_the_other() {
    for held in [json!({"hold": "fetch", "status": 200, "json": {"projects": []}}), json!({"reject": true}), json!({"status": 503})] {
        let Some(out) = run_at("/reviews", json!([inbox(), held.clone()]), json!([{"submit": TOKEN}, {"nav": "a[href=\"/dashboard\"]"}])) else { return };
        let text = shown(&out["frames"][2]);
        assert!(!text.contains("first-feature") && !text.contains("approved"), "{held}: reviews on the dashboard: {text}");
        assert_ownership(&out["frames"][2], &held, &text);
    }
    for held in [json!({"hold": "fetch", "status": 200, "json": {"reviews": []}}), json!({"reject": true}), json!({"status": 503})] {
        let Some(out) = run(json!([projects(), held.clone()]), json!([{"submit": TOKEN}, {"go": "/reviews"}])) else { return };
        let text = shown(&out["frames"][2]);
        assert!(!text.contains("Failed (24h)") && !text.contains("Unavailable"), "{held}: projects in the inbox: {text}");
        assert_ownership(&out["frames"][2], &held, &text);
    }
}

#[test]
fn unusable_project_data_gives_an_error_not_cards() {
    let bad = [
        json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true}]}}),
        json!({"status": 200, "json": {"projects": [{"id": "../x", "name": "x", "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null}]}}),
        json!({"status": 200, "badjson": true}),
    ];
    for reply in bad {
        let Some(out) = run(json!([reply.clone()]), json!([{"submit": TOKEN}])) else { return };
        let frame = &out["frames"][1];
        assert_eq!(frame["settled"]["cards"], 0, "{reply}: {out}");
        assert!(shown(frame).contains("could not be read"), "{reply}: {out}");
        assert!(frame["settled"]["inputs"].as_array().unwrap().is_empty(), "{reply}: the token is kept");
    }
}

#[test]
fn a_refused_token_on_the_dashboard_asks_for_the_token_again() {
    let Some(out) = run(json!([{"status": 401}]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["inputs"].as_array().unwrap().len(), 1, "{out}");
    assert!(shown(&out["frames"][1]).contains("Could not sign in"), "{out}");
}

#[test]
fn a_failed_dashboard_refresh_keeps_the_counts_marked_outdated_and_a_retry_replaces_them() {
    let fresh = json!({"status": 200, "json": {"projects": [{"id": "beta", "name": "beta", "available": true, "features": 9, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    for failure in [json!({"status": 503}), json!({"reject": true})] {
        let Some(out) = run(json!([projects(), failure.clone(), fresh.clone()]), json!([{"submit": TOKEN}, {"nav": "[data-refresh]"}, {"click": "button", "index": 0}])) else { return };
        let kept = shown(&out["frames"][2]);
        assert!(kept.contains("Features") && kept.contains("alpha") && kept.contains("Outdated"), "{failure}: {kept}");
        let after = shown(&out["frames"][3]);
        assert!(after.contains("beta") && after.contains("9 | Features") && !after.contains("alpha") && !after.contains("Outdated"), "{failure}: {after}");
    }
}

#[test]
fn each_project_card_shows_its_name_and_counts_and_the_page_says_when_it_last_updated() {
    let Some(out) = run(json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    let frame = &out["frames"][1];
    let text = shown(frame);
    assert!(text.contains("alpha") && text.contains("2 | Active runs"), "{text}");
    let checked = frame["settled"]["checked"].as_array().unwrap();
    assert_eq!(checked.len(), 1, "one Last checked line on the Dashboard: {out}");
    assert!(checked[0].as_str().unwrap().starts_with("Last checked"), "{out}");
    let times = frame["settled"]["times"].as_array().unwrap();
    assert!(times.iter().any(|t| t["label"] == "Last checked" && t["datetime"].as_str().is_some_and(|d| d.ends_with('Z'))), "an absolute time backs it: {out}");
}
