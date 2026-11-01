use orq_git::{detect_provider, parse_forge, remote_host, Provider, ProviderError, ProviderHosts};

fn detect(url: &str) -> Result<Provider, ProviderError> {
    detect_provider(url, &ProviderHosts::default(), None)
}

#[test]
fn github_and_gitlab_remotes_are_recognised_in_every_url_form() {
    for url in [
        "https://github.com/o/r.git",
        "http://github.com/o/r",
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
        "ssh://git@github.com:22/o/r.git",
        "git://github.com/o/r",
        "https://user:token@github.com/o/r",
        "https://github.com:443/o/r",
        "HTTPS://GitHub.COM/o/r",
        "https://github.com./o/r",
        "  https://github.com/o/r\n",
    ] {
        assert_eq!(detect(url), Ok(Provider::GitHub), "{url:?}");
    }
    for url in [
        "https://gitlab.com/group/sub/r.git",
        "git@gitlab.com:group/sub/r.git",
        "ssh://git@gitlab.com:2222/group/r.git",
    ] {
        assert_eq!(detect(url), Ok(Provider::GitLab), "{url:?}");
    }
}

#[test]
fn self_hosted_hosts_come_from_config_and_match_case_insensitively() {
    let hosts = ProviderHosts {
        github: vec!["github.corp.example".to_string()],
        gitlab: vec!["Git.Example.ORG".to_string()],
    };

    assert_eq!(
        detect_provider("git@git.example.org:team/r.git", &hosts, None),
        Ok(Provider::GitLab)
    );
    assert_eq!(
        detect_provider("https://GITHUB.corp.example/o/r", &hosts, None),
        Ok(Provider::GitHub)
    );
    // Without the config the same hosts are unknown.
    assert_eq!(
        detect("git@git.example.org:team/r.git"),
        Err(ProviderError::UnknownHost {
            host: "git.example.org".to_string()
        })
    );
}

#[test]
fn an_unknown_host_is_an_error_not_a_guess() {
    assert_eq!(
        detect("https://bitbucket.org/o/r.git"),
        Err(ProviderError::UnknownHost {
            host: "bitbucket.org".to_string()
        })
    );
}

#[test]
fn lookalike_hosts_never_match_a_known_provider() {
    for (url, host) in [
        ("https://github.com.evil.example/o/r", "github.com.evil.example"),
        ("https://notgithub.com/o/r", "notgithub.com"),
        ("git@gitlab.com.evil.example:o/r.git", "gitlab.com.evil.example"),
        ("https://evil.example/github.com/o/r", "evil.example"),
        ("https://github.com@evil.example/o/r", "evil.example"),
        ("https://github.com:pw@evil.example/o/r", "evil.example"),
        ("ssh://github.com@evil.example:22/o/r", "evil.example"),
    ] {
        assert_eq!(
            detect(url),
            Err(ProviderError::UnknownHost { host: host.to_string() }),
            "{url:?}"
        );
    }
}

#[test]
fn a_remote_without_a_host_is_reported_as_such() {
    for url in ["", "   ", "/srv/git/repo.git", "../repo", "./repo.git", "file:///srv/repo.git", "https://", "https:///o/r"] {
        assert_eq!(
            detect(url),
            Err(ProviderError::NoHost { url: url.to_string() }),
            "{url:?}"
        );
    }
}

#[test]
fn remote_host_extracts_the_lowercase_hostname_only() {
    assert_eq!(remote_host("git@GitLab.com:g/r.git").as_deref(), Some("gitlab.com"));
    assert_eq!(remote_host("ssh://u@h.example:2222/x").as_deref(), Some("h.example"));
    assert_eq!(remote_host("https://u:p@h.example:8443/x").as_deref(), Some("h.example"));
    assert_eq!(remote_host("/local/path"), None);
}

#[test]
fn an_explicit_forge_overrides_the_url_and_even_a_missing_host() {
    let hosts = ProviderHosts::default();
    assert_eq!(
        detect_provider("https://gitlab.com/g/r", &hosts, Some("gh")),
        Ok(Provider::GitHub)
    );
    assert_eq!(
        detect_provider("https://bitbucket.org/o/r", &hosts, Some("glab")),
        Ok(Provider::GitLab)
    );
    assert_eq!(detect_provider("", &hosts, Some(" GitHub ")), Ok(Provider::GitHub));
    assert_eq!(detect_provider("/local", &hosts, Some("gitlab")), Ok(Provider::GitLab));
    // An empty override means "not set".
    assert_eq!(
        detect_provider("https://github.com/o/r", &hosts, Some("")),
        Ok(Provider::GitHub)
    );
}

#[test]
fn an_unrecognised_forge_is_an_error_even_when_the_url_would_match() {
    assert_eq!(
        detect_provider("https://github.com/o/r", &ProviderHosts::default(), Some("bitbucket")),
        Err(ProviderError::InvalidForge("bitbucket".to_string()))
    );
    assert_eq!(parse_forge("gh"), Ok(Provider::GitHub));
    assert_eq!(parse_forge("GLAB"), Ok(Provider::GitLab));
    assert!(parse_forge("git").is_err());
}

#[test]
fn a_host_configured_for_both_providers_is_ambiguous() {
    let hosts = ProviderHosts {
        github: vec!["both.example".to_string()],
        gitlab: vec!["BOTH.example".to_string(), "github.com".to_string()],
    };
    assert_eq!(
        detect_provider("https://both.example/o/r", &hosts, None),
        Err(ProviderError::AmbiguousHost {
            host: "both.example".to_string()
        })
    );
    // Even a built-in host is ambiguous once config claims it for the other provider.
    assert_eq!(
        detect_provider("https://github.com/o/r", &hosts, None),
        Err(ProviderError::AmbiguousHost {
            host: "github.com".to_string()
        })
    );
    // An explicit forge still settles it.
    assert_eq!(
        detect_provider("https://both.example/o/r", &hosts, Some("gh")),
        Ok(Provider::GitHub)
    );
}

#[test]
fn each_provider_names_its_cli_and_errors_explain_themselves() {
    assert_eq!(Provider::GitHub.cli(), "gh");
    assert_eq!(Provider::GitLab.cli(), "glab");
    assert_eq!(
        ProviderError::UnknownHost { host: "x.example".to_string() }.to_string(),
        "unknown git host \"x.example\": set the forge explicitly or list the host in the settings"
    );
    assert_eq!(
        ProviderError::NoHost { url: "/srv/r.git".to_string() }.to_string(),
        "remote \"/srv/r.git\" has no host to detect a provider from"
    );
    assert_eq!(
        ProviderError::InvalidForge("x".to_string()).to_string(),
        "forge must be gh or glab, got \"x\""
    );
}

#[test]
fn a_bracketed_host_must_be_an_ipv6_address_followed_by_nothing_or_a_numeric_port() {
    for url in [
        "ssh://[2001:db8::1]junk/repo",
        "ssh://[2001:db8::1]:invalid/repo",
        "ssh://[not-an-ip]/repo",
        "ssh://[github.com]/repo",
        "ssh://[2001:db8::1/repo",
        "ssh://[]/repo",
    ] {
        assert_eq!(
            remote_host(url),
            None,
            "{url:?} should not yield a host"
        );
    }
    let hosts = ProviderHosts {
        github: vec![],
        gitlab: vec!["[2001:db8::1]".to_string()],
    };
    for url in [
        "ssh://[2001:db8::1]/repo",
        "ssh://[2001:DB8::1]:2222/repo",
        "https://u:p@[2001:0db8:0:0:0:0:0:1]:8443/repo",
    ] {
        assert_eq!(detect_provider(url, &hosts, None), Ok(Provider::GitLab), "{url:?}");
    }
    assert_eq!(remote_host("ssh://[2001:0DB8:0:0:0:0:0:1]/r").as_deref(), Some("[2001:db8::1]"));
}

#[test]
fn an_scp_like_remote_may_have_a_bracketed_ipv6_host() {
    let hosts = ProviderHosts {
        github: vec![],
        gitlab: vec!["[2001:db8::1]".to_string()],
    };
    for url in [
        "git@[2001:db8::1]:team/repo.git",
        "[2001:db8::1]:team/repo.git",
        "git@[2001:DB8:0:0:0:0:0:1]:team/repo.git",
    ] {
        assert_eq!(remote_host(url).as_deref(), Some("[2001:db8::1]"), "{url:?}");
        assert_eq!(detect_provider(url, &hosts, None), Ok(Provider::GitLab), "{url:?}");
    }
    for url in [
        "git@[2001:db8::1:team/repo.git",
        "git@[2001:db8::1]team/repo.git",
        "git@[not-ip]:team/repo.git",
    ] {
        assert_eq!(remote_host(url), None, "{url:?}");
    }
    // Plain scp-like remotes are unaffected.
    assert_eq!(remote_host("git@github.com:o/r.git").as_deref(), Some("github.com"));
}

fn no_host_url(url: &str) -> String {
    match detect(url) {
        Err(ProviderError::NoHost { url }) => url,
        other => panic!("expected NoHost for {url:?}, got {other:?}"),
    }
}

#[test]
fn credentials_never_appear_in_a_no_host_error_or_its_output() {
    for (url, secret, kept) in [
        ("https://user:secret@:443/repo.git", "secret", "https://***@:443/repo.git"),
        ("https://TOKEN123@/o/r", "TOKEN123", "https://***@/o/r"),
        ("https://u:p@ss@:80/x", "p@ss", "https://***@:80/x"),
        ("ssh://deploy:hunter2@/srv/r.git", "hunter2", "ssh://***@/srv/r.git"),
        ("git@:o/r", "git@", "***@:o/r"),
        ("HTTPS://x:pw9@?q=1", "pw9", "HTTPS://***@?q=1"),
    ] {
        let error = detect(url).unwrap_err();
        assert_eq!(no_host_url(url), kept, "{url:?}");
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(secret), "{secret:?} leaked in {text:?}");
        }
    }
    // Nothing is rewritten when there is no userinfo to hide.
    for url in ["", "   ", "/srv/git/a@b/repo.git", "file:///srv/repo.git", "https:///o/r", "../a@b", "https://:80/x@y"] {
        assert_eq!(no_host_url(url), url, "{url:?}");
    }
    // A host that is merely unknown never carries userinfo either.
    let unknown = detect("https://user:secret@bitbucket.org/o/r").unwrap_err();
    assert_eq!(unknown, ProviderError::UnknownHost { host: "bitbucket.org".to_string() });
    assert!(!format!("{unknown} {unknown:?}").contains("secret"));
}

#[test]
fn a_slash_before_the_scp_separator_makes_a_local_path_even_inside_brackets() {
    for url in [
        "[dir/file]@github.com:repo.git",
        "[a/b]:c",
        "git@[2001:db8::1/64]:team/repo.git",
        "./[dir]@github.com:repo.git",
    ] {
        assert_eq!(remote_host(url), None, "{url:?}");
        assert!(matches!(detect(url), Err(ProviderError::NoHost { .. })), "{url:?}");
    }
    // A slash after the separator is just the path.
    assert_eq!(remote_host("git@[2001:db8::1]:team/repo.git").as_deref(), Some("[2001:db8::1]"));
}
