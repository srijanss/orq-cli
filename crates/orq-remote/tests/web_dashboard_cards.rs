//! Project cards show compact stats, and the project last opened is marked on the Dashboard.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

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
    let project = |id: &str, active: u64| json!({"id": id, "name": id, "available": true, "features": 3, "active_runs": active, "reviews_waiting": 1, "failed_runs": 4, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha", 2), project("beta", 7)]}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_card_shows_each_count_as_a_value_above_its_short_label() {
    let Some(out) = run("/dashboard", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    let texts = texts(&out["frames"][1]);
    let start = texts.iter().position(|t| t == "alpha").unwrap();
    assert_eq!(texts[start + 1..start + 5], ["3", "Features", "2", "Active runs"], "{texts:?}");
    assert_eq!(texts[start + 6], "Reviews waiting", "the reviews waiting sit above their label: {texts:?}");
}

#[test]
fn the_project_last_opened_is_marked_on_the_dashboard() {
    let reviews = json!({"status": 200, "json": {"reviews": []}});
    let Some(out) = run("/projects/beta/reviews", json!([reviews, projects(), projects()]), json!([{"submit": TOKEN}, {"go": "/dashboard"}])) else { return };
    assert_eq!(out["frames"][2]["settled"]["selected"], json!(["beta"]), "{out}");
    let Some(fresh) = run("/dashboard", json!([projects()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(fresh["frames"][1]["settled"]["selected"], json!([]), "nothing is marked before a project was opened: {fresh}");
}

#[test]
fn the_sidebar_stays_dark_in_the_light_theme() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase();
    assert!(css.matches("--sidebar: #0f172a").count() >= 2, "the light theme gives the sidebar a dark colour");
    let start = css.find(".sidebar {").unwrap();
    let rule = &css[start..start + css[start..].find('}').unwrap()];
    for token in ["--text: var(--sidebar-text)", "--text-muted: var(--sidebar-muted)", "--border: var(--sidebar-border)"] {
        assert!(rule.contains(token), "the sidebar reads its own light-on-dark {token}: {rule}");
    }
}
