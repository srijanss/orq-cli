//! The sign-in page's identity, and how long a sign-in lasts.

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

#[test]
fn the_sign_in_page_shows_the_logo_and_the_product_name() {
    let Some(out) = run("/", json!([]), json!([])) else { return };
    let start = &out["frames"][0]["settled"];
    assert_eq!(start["brandLogos"], 1, "{out}");
    let texts: Vec<&str> = start["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    assert!(texts.contains(&"Orq"), "{texts:?}");
}
