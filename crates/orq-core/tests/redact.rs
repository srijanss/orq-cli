use orq_core::redact::{redact, REDACTED};

#[test]
fn credentials_private_keys_and_tokens_are_removed_and_ordinary_text_is_kept() {
    let pem = "\x2D----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
    let secrets = [
        pem,
        "gh\x70_abcdefghijklmnopqrstuvwxyz0123456789",
        "github\x5Fpat_11ABCDEFG0abcdefghijklmnop_qrstuvwxyz0123456789",
        "glpat\x2Dabcdefghij0123456789",
        "AK\x49AIOSFODNN7EXAMPLE",
        "xo\x78b-123456789012-abcdefghijklmnop",
        "s\x6B-abcdefghijklmnopqrstuvwxyz012345",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
    ];
    for secret in secrets {
        let out = redact(&format!("before {secret} after"));
        assert!(!out.contains(secret), "{secret:?} survived: {out}");
        assert!(out.starts_with("before ") && out.ends_with(" after"), "{out}");
        assert!(out.contains(REDACTED));
    }

    for line in [
        "Authorization: Bearer abc123def456ghi789",
        "password=hunter2hunter2",
        "API_KEY: s3cr3tvalue99",
        "export GITLAB_TOKEN=abcd1234efgh",
        "client_secret = \"shh-its-a-secret\"",
        "git clone https://user:pa55w0rd@example.com/repo.git",
    ] {
        let out = redact(line);
        for leaked in ["abc123def456ghi789", "hunter2hunter2", "s3cr3tvalue99", "abcd1234efgh", "shh-its-a-secret", "pa55w0rd"] {
            assert!(!out.contains(leaked), "{leaked} survived in {out:?}");
        }
        assert!(out.contains(REDACTED), "{out}");
    }
    // The name of a secret-bearing key stays so the log remains readable.
    assert!(redact("password=hunter2hunter2").starts_with("password="));

    for plain in ["running 12 tests: all passed", "fix(core): tighten modes", "see https://example.com/docs/page", "token count was 42", "the password field is required"] {
        assert_eq!(redact(plain), plain);
    }
    let once = redact("password=hunter2hunter2 and gh\x70_abcdefghijklmnopqrstuvwxyz0123456789");
    assert_eq!(redact(&once), once, "redaction is idempotent");
}
