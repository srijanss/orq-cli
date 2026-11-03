use orq_core::redact::redact_bytes;
use serde_json::{json, Value};

fn run(input: &str) -> Value {
    serde_json::from_slice(&redact_bytes(input.as_bytes())).unwrap()
}

#[test]
fn numbers_arrays_and_objects_under_secret_keys_are_replaced() {
    let out = run(r#"{"password":123456,"api_key":["custom-value"],"token":{"v":"abc12345"},"ok":7}"#);
    assert_eq!(out["password"], "[REDACTED]");
    assert_eq!(out["api_key"], "[REDACTED]");
    assert_eq!(out["token"], "[REDACTED]");
    assert_eq!(out["ok"], 7);
}

#[test]
fn booleans_and_null_under_secret_keys_are_kept() {
    let out = run(r#"{"token":null,"has_password":true}"#);
    assert_eq!(out, json!({"token": null, "has_password": true}));
}

#[test]
fn the_same_holds_for_jsonl_records() {
    let text = String::from_utf8(redact_bytes(b"{\"secret\":42}\n{\"n\":1}\n")).unwrap();
    assert!(!text.contains("42"), "{text}");
    assert!(text.contains("{\"n\":1}"));
}
