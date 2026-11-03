use orq_core::redact::{redact, REDACTED};

#[test]
fn values_of_quoted_secret_keys_are_removed_and_other_fields_are_kept() {
    for (input, leaked) in [
        (r#"{"password":"hunter2hunter2","user":"alice"}"#, "hunter2hunter2"),
        (r#"{"api_key": "custom-secret-value", "user": "alice"}"#, "custom-secret-value"),
        (r#"'client_secret': 'shh-its-a-secret'"#, "shh-its-a-secret"),
        (r#"{"GITHUB_TOKEN" : "abcd1234efgh"}"#, "abcd1234efgh"),
    ] {
        let out = redact(input);
        assert!(!out.contains(leaked), "{leaked} survived in {out}");
        assert!(out.contains(REDACTED), "{out}");
    }
    let out = redact(r#"{"password":"hunter2hunter2","user":"alice"}"#);
    assert!(out.contains(r#""user":"alice""#), "unrelated field lost: {out}");
    assert_eq!(redact(r#"{"summary":"token count was 42"}"#), r#"{"summary":"token count was 42"}"#);
}
