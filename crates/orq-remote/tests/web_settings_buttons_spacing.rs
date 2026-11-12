//! Settings' buttons keep clear of the card's lines: an agent card's Edit sits inside its header with space
//! above and below rather than touching the header's borders, and Sign out everywhere carries the sign-out
//! icon like Sign out, so the two account buttons line up.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const SELECTORS: [&str; 2] = [".settings-row > button[data-sign-out-everywhere] > svg.icon:first-child", ".settings-row > button[data-sign-out] > svg.icon:first-child"];

#[test]
fn sign_out_everywhere_carries_the_sign_out_icon_like_sign_out() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let settings = json!({"status": 200, "json": {"settings": {"implementer": {"harness": "claude", "model": "sonnet", "effort": null}, "reviewer": null, "models": [], "remote_actions": false, "version": "v-1"}}});
    let scenario = json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": {"/auth/passkeys": {"passkeys": []}}, "auth_failures": {},
        "responses": [settings], "steps": [], "selectors": SELECTORS
    });
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap();
    for selector in SELECTORS {
        assert_eq!(last["settled"]["matches"][selector].as_u64().unwrap(), 1, "{selector}: {out}");
    }
}

#[test]
fn an_agent_cards_header_leaves_room_above_and_below_its_edit_button() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let top = css.lines().find(|line| line.starts_with(".settings-card-top {")).expect("a .settings-card-top rule");
    assert!(top.contains("padding: 0.375rem 1rem 0.375rem 0;"), "{top}");
}
