use orq_git::redact_urls;

#[test]
fn credentials_and_query_secrets_are_removed_from_every_url_in_a_diagnostic() {
    let cases = [
        ("fatal: unable to access 'http://user:pw@host/r.git/': nope", "host/r.git/"),
        ("see https://host/r.git?access_token=abc123#frag now", "https://host/r.git now"),
        ("a http://u:p@one/x and ssh://git:tok@two/y", "http://one/x and ssh://two/y"),
        ("fatal: 'http://user:sec'ret@host/repo' failed", "http://host/repo"),
        ("http://u:p'q?x='y&z=w@host/path", "http://host/path"),
        ("no urls here: just text", "no urls here: just text"),
    ];
    for (input, expected) in cases {
        let out = redact_urls(input);
        for secret in ["pw", "abc123", "tok", "sec'ret", "ret", "p'q", "u:p", "access_token"] {
            if !expected.contains(secret) {
                assert!(!out.contains(secret), "{input:?} -> {out:?} leaks {secret:?}");
            }
        }
        assert!(out.contains(expected), "{input:?} -> {out:?}, wanted {expected:?}");
    }
}
