use orq_core::redact::redact_bytes;

#[test]
fn a_large_plain_log_with_a_multiline_key_is_redacted_whole_and_idempotently() {
    let filler = "build output line 12345: ok\n".repeat(20_000);
    let log = format!("{filler}\x2D----BEGIN PRIVATE KEY-----\nAAAA1111\n-----END PRIVATE KEY-----\npassword=hunter2hunter2\n{filler}");
    let once = redact_bytes(log.as_bytes());
    let text = String::from_utf8(once.clone()).unwrap();
    assert!(!text.contains("AAAA1111") && !text.contains("hunter2hunter2"));
    assert!(text.starts_with(&filler) && text.ends_with(&filler));
    assert_eq!(redact_bytes(&once), once);
}

#[test]
fn a_secret_free_plain_log_comes_back_byte_for_byte() {
    let log = "plain line\r\nanother 42\n\nlast without newline";
    assert_eq!(redact_bytes(log.as_bytes()), log.as_bytes());
}
