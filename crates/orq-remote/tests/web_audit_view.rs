//! The Audit page under Settings: the recent remote actions, read only — when, who, what and how it ended.

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

fn settings() -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true}}})
}

fn audit(lines: Value) -> Value {
    json!({"status": 200, "json": {"audit": lines}})
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn lines() -> Value {
    json!([
        {"time": 1793700000, "identity": "web", "action": "run.start", "project": "app", "run": "run-9", "outcome": "allowed", "reason": null},
        {"time": 1793600000, "identity": "web", "action": "run.pause", "project": "<b>app</b>", "run": null, "outcome": "refused", "reason": "r-1 is failed"},
        {"damaged": true}
    ])
}

#[test]
fn the_audit_page_asks_for_the_audit_once_and_lists_when_who_what_and_the_outcome() {
    let Some(out) = run("/settings/audit", json!([audit(lines())]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["requests"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["requests"][0]["url"], "/api/audit", "{out}");
    assert_eq!(out["requests"][0]["method"], "GET", "{out}");
    let t = shown(&out["frames"][1]);
    for text in ["Audit", "web", "run.start", "app", "run-9", "allowed", "refused", "r-1 is failed", "<b>app</b>", "Unreadable entry"] {
        assert!(t.iter().any(|x| x.contains(text)), "{text} missing: {t:?}");
    }
    let times: Vec<&str> = out["frames"][1]["settled"]["times"].as_array().unwrap().iter().filter_map(|x| x["datetime"].as_str()).collect();
    assert_eq!(times[..2], ["2026-11-03T10:00:00Z", "2026-11-02T06:13:20Z"]);
    assert_eq!(out["frames"][1]["settled"]["buttons"], json!([]), "{out}");
    assert_eq!(out["frames"][1]["settled"]["inputs"], json!([]), "{out}");
}

#[test]
fn an_empty_audit_says_so_and_unusable_data_draws_nothing() {
    let Some(out) = run("/settings/audit", json!([audit(json!([]))]), json!([{"submit": TOKEN}])) else { return };
    assert!(shown(&out["frames"][1]).iter().any(|x| x.contains("No actions recorded yet")), "{out}");
    let Some(out) = run("/settings/audit", json!([{"status": 200, "json": {"audit": "x"}}]), json!([{"submit": TOKEN}])) else { return };
    assert!(!shown(&out["frames"][1]).iter().any(|x| x.contains("No actions recorded yet") || x == "web"), "{out}");
}

#[test]
fn the_settings_page_links_to_the_audit_and_other_settings_addresses_stay_not_found() {
    let Some(out) = run("/settings", json!([settings()]), json!([{"submit": TOKEN}])) else { return };
    assert_eq!(out["frames"][1]["settled"]["links"].as_array().unwrap().iter().map(|l| l["href"].clone()).collect::<Vec<_>>(), [json!("/settings/audit")], "{out}");
    let Some(out) = run("/settings/audit", json!([audit(json!([]))]), json!([{"submit": TOKEN}, {"go": "/settings/audit/x"}])) else { return };
    assert!(shown(&out["frames"][2]).iter().any(|x| x == "Not found"), "{out}");
}

#[test]
fn the_server_answers_the_audit_address_with_the_page() {
    let page = orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", "/").unwrap().body;
    assert_eq!(orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", "/settings/audit").map(|a| a.body), Some(page));
    assert!(orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", "/settings/audit/x").is_none());
}

#[test]
fn an_entry_with_a_time_the_page_cannot_draw_is_an_unreadable_entry_and_the_rest_still_show() {
    let odd = json!([
        {"time": 8640000000001_i64, "identity": "web", "action": "run.start", "project": "app", "run": null, "outcome": "allowed", "reason": null},
        {"time": 1793700000, "identity": "web", "action": "run.pause", "project": "app", "run": null, "outcome": "allowed", "reason": null}
    ]);
    let Some(out) = run("/settings/audit", json!([audit(odd)]), json!([{"submit": TOKEN}])) else { return };
    let t = shown(&out["frames"][1]);
    assert!(t.iter().any(|x| x == "Unreadable entry"), "{t:?}");
    assert!(t.iter().any(|x| x == "run.pause"), "{t:?}");
}
