//! The Audit page reads as one entry per action, not a run-on line: the action in bold with its outcome as a
//! badge (allowed in the completed colour, refused in the failed colour) and the time on its top line; who,
//! the project and the run as labelled pairs below; a refusal's reason on its own line. The pairs wrap, so a
//! phone shows them stacked instead of crammed together.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = [
    "ul.audit-lines > li.audit-entry .audit-head strong.audit-action",
    ".audit-head .badge.badge-status-completed",
    ".audit-head .badge.badge-status-failed",
    ".audit-head time",
    "dl.audit-meta > div > dt + dd",
    ".audit-entry > .audit-reason",
    "li.audit-entry.audit-damaged",
];

fn run() -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let lines = json!([
        {"time": 1793700000, "identity": "web", "action": "run.start", "project": "app", "run": "run-9", "outcome": "allowed", "reason": null},
        {"time": 1793600000, "identity": "web", "action": "run.pause", "project": "app", "run": null, "outcome": "refused", "reason": "r-1 is failed"},
        {"damaged": true}
    ]);
    let scenario = json!({"url": "/settings/audit", "responses": [{"status": 200, "json": {"audit": lines}}], "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

#[test]
fn each_audit_entry_has_a_head_line_labelled_pairs_and_its_reason_on_its_own_line() {
    let Some(out) = run() else { return };
    let shown = out["frames"].as_array().unwrap().last().unwrap();
    for (selector, expected) in SELECTORS.iter().zip([2, 1, 1, 2, 5, 1, 1]) {
        assert_eq!(count(shown, selector), expected, "{selector}: {out}");
    }
    let labels: Vec<&str> = shown["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    for label in ["By", "Project", "Run"] {
        assert!(labels.contains(&label), "{label}: {labels:?}");
    }
}

#[test]
fn the_labelled_pairs_wrap_on_a_narrow_screen() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let rule = css.lines().find(|line| line.starts_with(".audit-meta {")).expect("an .audit-meta rule");
    assert!(rule.contains("flex-wrap: wrap"), "{rule}");
    assert!(css.lines().any(|line| line.starts_with(".audit-entry {")), "an .audit-entry rule");
    let label = css.lines().find(|line| line.starts_with(".audit-meta dt {")).expect("an .audit-meta dt rule");
    assert!(label.contains("flex: none") && label.contains("white-space: nowrap"), "a long value never squeezes its label: {label}");
}
