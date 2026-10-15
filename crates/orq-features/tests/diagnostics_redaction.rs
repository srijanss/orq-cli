use orq_features::redact_secrets;

fn gone(input: &str, secret: &str) {
    let out = redact_secrets(input);
    assert!(!out.contains(secret), "{secret} survived in {out:?}");
    assert!(out.contains("[REDACTED]"), "no marker in {out:?}");
}

#[test]
fn bearer_and_authorization_values_are_removed() {
    gone("Authorization: Bearer abc123def456", "abc123def456");
    gone("curl -H 'authorization: Basic dXNlcjpwYXNz'", "dXNlcjpwYXNz");
}

#[test]
fn environment_style_assignments_lose_their_values() {
    let out = redact_secrets("ANTHROPIC_API_KEY=sk-ant-api03-zzzzzzzzzz HOME=/home/dev");
    assert!(!out.contains("sk-ant"), "{out:?}");
    assert!(!out.contains("/home/dev"), "env values are redacted: {out:?}");
    assert!(out.contains("ANTHROPIC_API_KEY=[REDACTED]"), "{out:?}");
}

#[test]
fn credential_named_keys_lose_their_values_in_every_common_shape() {
    gone("password=hunter2", "hunter2");
    gone("{\"client_secret\": \"s3cr3tvalue\"}", "s3cr3tvalue");
    gone("--api-key abcdef123 --model x", "abcdef123");
    gone("oauth_token: tok-value-1", "tok-value-1");
}

#[test]
fn well_known_token_shapes_are_removed_anywhere() {
    gone("failed with ghp_abcdefghijklmnopqrstuvwxyz0123456789", "ghp_abcdefghijklmnopqrstuvwxyz0123456789");
    gone("token sk-live-1234567890abcdef", "sk-live-1234567890abcdef");
    gone("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijk", "eyJhbGciOiJIUzI1NiJ9");
    gone("blob 0123456789abcdef0123456789abcdef0123456789abcdef", "0123456789abcdef0123456789abcdef");
}

#[test]
fn ordinary_diagnostic_text_is_untouched() {
    let text = "error: could not read /repo/docs/SPEC.md at line 3\nthe agent exited with status 1\n";
    assert_eq!(redact_secrets(text), text);
}

#[test]
fn line_structure_is_preserved() {
    let out = redact_secrets("a\nPASSWORD=x\n\nb\n");
    assert_eq!(out.lines().count(), 4, "{out:?}");
}
