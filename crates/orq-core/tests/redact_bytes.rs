use orq_core::redact::{redact, redact_bytes};

#[test]
fn a_secret_next_to_invalid_utf8_is_still_removed() {
    let mut input = b"\xff\xfe before password=hunter2hunter2 ".to_vec();
    input.extend_from_slice(b"\x80 gh\x70_abcdefghijklmnopqrstuvwxyz0123456789 \xc3");
    let out = String::from_utf8_lossy(&redact_bytes(&input)).into_owned();
    assert!(!out.contains("hunter2hunter2") && !out.contains("gh\x70_abcdefghijklmnopqrstuvwxyz0123456789"), "{out}");
    assert!(out.contains("before"));
}

#[test]
fn a_private_key_without_an_end_marker_is_removed_to_the_end_of_the_text() {
    let out = redact("start\n\x2D----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\nmore base64");
    assert!(out.starts_with("start\n"));
    assert!(!out.contains("MIIEvQIBADANBgkqhkiG9w0B") && !out.contains("more base64"), "{out}");
    let out = String::from_utf8(redact_bytes(b"x\n\x2D----BEGIN EC PRIVATE KEY-----\nAAAA")).unwrap();
    assert!(!out.contains("AAAA"), "{out}");
}

#[test]
fn quotes_and_backslashes_in_plain_text_do_not_hide_a_secret() {
    for input in [r#"password=\"hunter2hunter2\""#, r#"echo "password=hunter2hunter2""#, r#"path=C:\dir password='hunter2hunter2'"#] {
        let out = redact(input);
        assert!(!out.contains("hunter2hunter2"), "{input} -> {out}");
    }
}
