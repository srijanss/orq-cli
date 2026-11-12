//! A feature's page and a run's page keep their action buttons apart: on a feature's page Edit, Start and
//! Start with… sit in one row with a gap between them and space above the tabs; on a run's page the actions
//! sit in a row with space between them and the title above and the start time below.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 2] = [
    ".feature-detail > .feature-detail-actions > .feature-editor > [data-feature-edit]",
    ".feature-detail > .feature-detail-actions > .run-action [data-start]",
];

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    css.lines().find(|line| line.starts_with(&format!("{selector} {{"))).unwrap_or_else(|| panic!("a {selector} rule"))
}

#[test]
fn a_features_page_puts_its_actions_in_one_row() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let ledger = json!({"status": 200, "json": {"features": [{"name": "next-one", "description": null, "status": "pending", "depends_on": [], "recorded_at": null}], "remote_actions": true}});
    let runs = json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0, "remote_actions": true}});
    let scenario = json!({"url": "/projects/alpha/features/next-one", "responses": [ledger, runs, projects()], "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap();
    for selector in SELECTORS {
        assert_eq!(last["settled"]["matches"][selector].as_u64().unwrap(), 1, "{selector}: {out}");
    }
}

#[test]
fn the_action_rows_have_a_gap_and_space_above_and_below() {
    let css = css();
    let row = rule(&css, ".feature-detail-actions");
    assert!(row.contains("display: flex") && row.contains("gap:") && row.contains("margin:"), "{row}");
    let head = rule(&css, ".feature-detail-title > .run-actions");
    assert!(head.contains("display: flex") && head.contains("margin:"), "{head}");
}
