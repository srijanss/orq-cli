use orq_core::redact::redact_bytes;

fn run(input: &str) -> String {
    String::from_utf8(redact_bytes(input.as_bytes())).unwrap()
}

#[test]
fn a_complete_private_key_between_json_lines_is_removed_whole() {
    let out = run("{\"a\":1}\n\x2D----BEGIN PRIVATE KEY-----\nAAAA1111\n12345678\n-----END PRIVATE KEY-----\n{\"b\":2}\n");
    assert!(!out.contains("AAAA1111") && !out.contains("12345678"), "{out}");
    assert!(out.starts_with("{\"a\":1}\n") && out.ends_with("{\"b\":2}\n"), "{out}");
}

#[test]
fn a_truncated_private_key_after_a_json_line_is_removed_to_the_end() {
    let out = run("{}\n\x2D----BEGIN PRIVATE KEY-----\nAAAA\n");
    assert!(!out.contains("AAAA"), "{out}");
    assert!(out.starts_with("{}\n"));
}

#[test]
fn plain_lines_around_json_lines_are_still_redacted_and_keep_their_order() {
    let out = run("first password=hunter2hunter2\n{\"msg\":\"ok\"}\nlast gh\x70_abcdefghijklmnopqrstuvwxyz0123456789");
    assert!(!out.contains("hunter2hunter2") && !out.contains("gh\x70_abcdef"), "{out}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("first ") && lines[1] == "{\"msg\":\"ok\"}" && lines[2].starts_with("last "));
}
