use orq_features::redact_secrets;

#[test]
fn compact_json_credentials_lose_their_values() {
    let out = redact_secrets(r#"{"client_secret":"s3cr3tvalue"}"#);

    assert!(!out.contains("s3cr3tvalue"), "{out}");
    assert!(out.contains("client_secret"), "{out}");
}

#[test]
fn only_the_sensitive_fields_of_a_json_object_are_redacted() {
    let out = redact_secrets(r#"{"user":"alice","password":"hunter2 and more","count":3}"#);

    assert!(!out.contains("hunter2"), "{out}");
    assert!(!out.contains("and more"), "{out}");
    assert!(out.contains("alice") && out.contains("count"), "{out}");
}

#[test]
fn quoted_multiword_values_are_redacted_whole() {
    for input in [
        r#"password="two words here""#,
        "token='two words here'",
        r#"--password "two words here""#,
        r#"secret: "two words here""#,
    ] {
        let out = redact_secrets(input);
        assert!(!out.contains("words"), "{input} -> {out}");
        assert!(!out.contains("two"), "{input} -> {out}");
    }
}

#[test]
fn quoted_prose_that_is_not_a_credential_is_kept() {
    let input = r#"the agent said "hello there" and then 'goodbye now'"#;

    assert_eq!(redact_secrets(input), input);
}

#[test]
fn escaped_quotes_inside_a_secret_do_not_end_the_redaction() {
    let out = redact_secrets(r#"{"token":"abc\"leakedtail"}"#);

    assert!(!out.contains("leakedtail"), "{out}");
}
