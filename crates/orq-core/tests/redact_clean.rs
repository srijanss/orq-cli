use orq_core::redact::{redact, redact_bytes};

#[test]
fn a_large_secret_free_text_comes_back_unchanged() {
    let text = "ordinary build output line 12345: all tests passed\n".repeat(20_000);
    assert_eq!(redact(&text), text);
    assert_eq!(redact_bytes(text.as_bytes()), text.as_bytes());
}

#[test]
fn a_secret_at_the_end_of_a_large_text_is_still_removed_and_the_rest_kept() {
    let filler = "ordinary build output line 12345: all tests passed\n".repeat(20_000);
    let out = redact(&format!("{filler}password=hunter2hunter2\n"));
    assert!(out.starts_with(&filler) && !out.contains("hunter2hunter2"));
}
