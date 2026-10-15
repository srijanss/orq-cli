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

fn delimited_key() -> String {
    format!("password{},suffix", "a".repeat(130))
}

#[test]
fn a_delimiter_inside_a_long_quoted_key_does_not_hide_the_credential() {
    let out = redact_secrets(&format!("{{\"{}\":\"hunter2\"}}", delimited_key()));
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn a_delimiter_inside_a_long_quoted_key_does_not_hide_a_clipped_value() {
    let out = redact_secrets(&format!("{{\"{}\":\"hunter2", delimited_key()));
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn whitespace_and_brackets_inside_a_quoted_key_are_part_of_the_key() {
    let key = format!("password {} [x]", "b".repeat(200));
    let out = redact_secrets(&format!("{{\"{key}\": \"hunter2\"}}"));
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn an_escaped_quote_inside_a_quoted_key_is_handled() {
    let key = format!("password\\\"{},x", "c".repeat(150));
    let out = redact_secrets(&format!("{{\"{key}\":\"hunter2\"}}"));
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn a_long_non_sensitive_quoted_key_keeps_its_value_visible() {
    let key = format!("note,{}", "d".repeat(150));
    let out = redact_secrets(&format!("{{\"{key}\":\"visible\"}}"));
    assert!(out.contains("visible"), "{out}");
}
