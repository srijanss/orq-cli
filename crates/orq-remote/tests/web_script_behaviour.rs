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
    let Some(out) = run(json!({"responses": [{"status": 200, "json": {"reviews": [{}, {}]}}], "steps": [{"submit": TOKEN}]})) else { return };

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
    assert!(out["frames"][1]["settled"]["texts"].to_string().contains("2 review(s)"), "{out}");
}

#[test]
fn an_empty_or_blank_entry_sends_nothing() {
    let Some(out) = run(json!({"responses": [], "steps": [{"submit": ""}, {"submit": "   "}]})) else { return };
    assert!(out["requests"].as_array().unwrap().is_empty(), "{out}");
}
