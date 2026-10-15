use orq_features::redact_secrets;

fn long_key() -> String {
    format!("password{}", "a".repeat(130))
}

#[test]
fn a_long_sensitive_key_still_redacts_its_complete_quoted_value() {
    let line = format!("{{\"{}\":\"hunter2\"}}", long_key());
    let out = redact_secrets(&line);
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn a_long_sensitive_key_still_redacts_a_clipped_quoted_value() {
    let line = format!("{{\"{}\":\"hunter2", long_key());
    let out = redact_secrets(&line);
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn a_long_sensitive_key_with_equals_and_spaces_in_the_value_is_redacted() {
    let line = format!("{}=\"correct horse battery\"", long_key());
    let out = redact_secrets(&line);
    assert!(!out.contains("horse"), "{out}");
}

#[test]
fn an_ordinary_short_quoted_value_is_untouched() {
    let out = redact_secrets("{\"name\":\"visible\"}");
    assert!(out.contains("visible"), "{out}");
}
