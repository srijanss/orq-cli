//! Runs the page script (`web/app.js`) under node with a fake page and a
//! scripted `fetch`, and checks what it actually does with the token.

use std::process::Command;

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

/// The harness output, or None when node is not installed.
fn run(scenario: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = match Command::new("node").arg(format!("{dir}/tests/web_harness.js")).arg(format!("{dir}/web/app.js")).arg(scenario.to_string()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: node is not installed");
            return None;
        }
        Err(error) => panic!("cannot run node: {error}"),
    };
    assert!(output.status.success(), "the harness failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).expect("the harness prints JSON"))
}

#[test]
fn a_typed_token_is_sent_once_as_a_bearer_header_on_a_get_to_the_inbox_and_the_field_is_emptied() {
    let Some(out) = run(json!({"responses": [{"status": 200, "json": {"reviews": [{"feature": "alpha"}, {"feature": "beta"}]}}], "steps": [{"submit": TOKEN}]})) else { return };

    // Before anything is typed there is a masked, non-storable field.
    let start = &out["frames"][0]["settled"];
    assert_eq!(start["inputs"][0]["type"], "password");
    assert_eq!(start["inputs"][0]["autocomplete"], "off");

    let requests = out["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{out}");
    assert_eq!(requests[0]["url"], "/api/reviews");
    assert_eq!(requests[0]["method"], "GET");
    assert_eq!(requests[0]["headers"], json!({"Authorization": format!("Bearer {TOKEN}")}));

    // The field was emptied the moment the token was taken, before the reply.
    assert_eq!(out["frames"][1]["fieldAfter"], "", "the typed field still holds the token");
    assert!(!out["frames"][1]["sync"].to_string().contains(TOKEN), "{out}");
    // The token appears nowhere on the page afterwards.
    assert!(!out["frames"][1]["settled"].to_string().contains(TOKEN), "{out}");
    assert!(out["frames"][1]["settled"]["texts"].to_string().contains("alpha"), "{out}");
}

#[test]
fn an_empty_or_blank_entry_sends_nothing() {
    let Some(out) = run(json!({"responses": [], "steps": [{"submit": ""}, {"submit": "   "}]})) else { return };
    assert!(out["requests"].as_array().unwrap().is_empty(), "{out}");
}

const NEXT_TOKEN: &str = "second-token-0123456789abcdef0123456789";

/// The texts shown on a settled frame, as one string.
fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn assert_asks_again(frame: &Value, what: &str) -> String {
    let inputs = frame["settled"]["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1, "{what}: the token field is shown again: {frame}");
    assert_eq!(inputs[0]["value"], "", "{what}: the field is empty");
    assert!(!frame["settled"].to_string().contains(TOKEN), "{what}: the old token is still on the page");
    shown(frame)
}

#[test]
fn a_refused_token_and_a_failed_request_both_clear_the_token_and_show_the_same_message() {
    let Some(refused) = run(json!({"responses": [{"status": 401}], "steps": [{"submit": TOKEN}]})) else { return };
    let Some(offline) = run(json!({"responses": [{"reject": true}], "steps": [{"submit": TOKEN}]})) else { return };
    let after_401 = assert_asks_again(&refused["frames"][1], "401");
    let after_failure = assert_asks_again(&offline["frames"][1], "rejected fetch");
    assert!(!after_401.trim().is_empty());
    assert_eq!(after_401, after_failure, "the two failures must look identical");
    let lower = after_401.to_lowercase();
    for blamed in ["wrong", "invalid", "incorrect", "expired", "network", "offline", "unreachable", "timeout", "refused"] {
        assert!(!lower.contains(blamed), "the message says {blamed}: {after_401}");
    }
}

#[test]
fn after_a_failed_sign_in_only_the_newly_typed_token_is_sent() {
    for first in [json!({"status": 401}), json!({"reject": true})] {
        let Some(out) = run(json!({"responses": [first, {"status": 200, "json": {"reviews": []}}], "steps": [{"submit": TOKEN}, {"submit": NEXT_TOKEN}]})) else { return };
        let requests = out["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 2, "{out}");
        assert_eq!(requests[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
        assert_eq!(requests[1]["headers"], json!({"Authorization": format!("Bearer {NEXT_TOKEN}")}), "{out}");
        assert!(!out["frames"][2]["settled"].to_string().contains(NEXT_TOKEN));
    }
}

#[test]
fn a_server_error_leaves_a_way_to_try_again_without_retyping_the_token() {
    for status in [429, 500, 503] {
        let scenario = json!({"responses": [{"status": status}, {"status": 200, "json": {"reviews": [{"feature": "gamma"}]}}], "steps": [{"submit": TOKEN}, {"click": "button"}]});
        let Some(out) = run(scenario) else { return };
        let failed = &out["frames"][1]["settled"];
        assert_eq!(failed["buttons"].as_array().unwrap().len(), 1, "{status}: a retry button is offered: {out}");
        assert!(!failed.to_string().contains(TOKEN), "{status}");
        // Retrying asks again with the token already held, and recovers.
        let requests = out["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 2, "{status}: {out}");
        assert_eq!(requests[1]["headers"], requests[0]["headers"], "{status}");
        assert_eq!(requests[1]["method"], "GET");
        assert!(out["frames"][2]["settled"]["texts"].to_string().contains("gamma"), "{status}: {out}");
    }
}
