use orq_core::redact::redact_bytes;

fn run(input: &str) -> String {
    String::from_utf8(redact_bytes(input.as_bytes())).unwrap()
}

#[test]
fn an_escaped_secret_in_a_string_record_is_removed() {
    let out = run("{\"a\":1}\n\"pass\\u0077ord=hunter2hunter2\"\n{\"b\":2}\n");
    assert!(!out.contains("hunter2hunter2"), "{out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(serde_json::from_str::<String>(lines[1]).is_ok(), "{out}");
}

#[test]
fn numeric_boolean_and_null_records_survive_unchanged_between_objects() {
    let input = "{\"a\":1}\n42\ntrue\nnull\n{\"b\":2}\n";
    assert_eq!(run(input), input);
}

#[test]
fn a_key_body_made_of_digits_is_removed_with_the_rest_of_the_key() {
    let out = run("{}\n\x2D----BEGIN PRIVATE KEY-----\n12345678\ntrue\nnull\n-----END PRIVATE KEY-----\n{}\n");
    assert!(!out.contains("12345678") && !out.contains("true") && !out.contains("null"), "{out}");
}
