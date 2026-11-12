//! On the Audit page only a refusal's reason is in the failed colour; an allowed entry's note (a renamed
//! passkey, say) is muted text, not an error.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 2] = ["li[data-outcome=refused] > p.audit-reason.audit-reason-refused", "li[data-outcome=allowed] > p.audit-reason:not(.audit-reason-refused)"];

#[test]
fn only_a_refused_entry_marks_its_reason_and_only_that_mark_is_red() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let rule = |start: &str| css.lines().find(|line| line.starts_with(start)).unwrap_or_else(|| panic!("a {start} rule")).to_string();
    assert!(rule(".audit-reason {").contains("var(--text-muted)"), "{}", rule(".audit-reason {"));
    assert!(rule(".audit-reason-refused {").contains("var(--status-failed)"));
    if !web_deps::ready() {
        return;
    }
    let lines = json!([
        {"time": 1793700000, "identity": "web", "action": "settings.passkey", "project": "app", "run": null, "outcome": "allowed", "reason": "renamed Passkey 1 to phone"},
        {"time": 1793600000, "identity": "web", "action": "run.pause", "project": "app", "run": null, "outcome": "refused", "reason": "r-1 is failed"}
    ]);
    let scenario = json!({"url": "/settings/audit", "responses": [{"status": 200, "json": {"audit": lines}}], "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let shown = out["frames"].as_array().unwrap().last().unwrap();
    for selector in SELECTORS {
        assert_eq!(shown["settled"]["matches"][selector], 1, "{selector}: {out}");
    }
}
