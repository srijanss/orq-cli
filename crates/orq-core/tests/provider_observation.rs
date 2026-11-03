use orq_core::no_merge::forbidden_provider_call;
use orq_core::provider_observation::{classify_failure, parse_view, view_command};
use orq_core::review_lifecycle::{ObservationError, ProviderState};

const HEAD: &str = "1111111111111111111111111111111111111111";

#[test]
fn github_view_is_a_read_only_pr_view() {
    let (program, args) = view_command("github", "owner/repo", "7").unwrap();
    assert_eq!(program, "gh");
    assert_eq!(args, ["pr", "view", "7", "--repo", "owner/repo", "--json", "state,headRefOid"]);
    assert!(forbidden_provider_call(&program, &args).is_none());
}

#[test]
fn gitlab_view_is_a_read_only_mr_view() {
    let (program, args) = view_command("gitlab", "group/repo", "12").unwrap();
    assert_eq!(program, "glab");
    assert_eq!(args, ["mr", "view", "12", "--repo", "group/repo", "--output", "json"]);
    assert!(forbidden_provider_call(&program, &args).is_none());
}

#[test]
fn an_unknown_provider_or_a_flag_shaped_id_is_refused() {
    assert!(view_command("bitbucket", "o/r", "1").is_err());
    assert!(view_command("github", "o/r", "--web").is_err());
    assert!(view_command("github", "--hostname=evil", "1").is_err());
}

#[test]
fn github_states_map_to_provider_states() {
    for (name, expected) in [("OPEN", ProviderState::Open), ("MERGED", ProviderState::Merged), ("CLOSED", ProviderState::Closed)] {
        let json = format!(r#"{{"state":"{name}","headRefOid":"{HEAD}"}}"#);
        let seen = parse_view("github", &json).unwrap();
        assert_eq!(seen.state, expected);
        assert_eq!(seen.head_sha, HEAD);
    }
}

#[test]
fn gitlab_states_map_to_provider_states() {
    for (name, expected) in [("opened", ProviderState::Open), ("merged", ProviderState::Merged), ("closed", ProviderState::Closed)] {
        let json = format!(r#"{{"state":"{name}","sha":"{HEAD}"}}"#);
        assert_eq!(parse_view("gitlab", &json).unwrap().state, expected);
    }
}

#[test]
fn gitlab_falls_back_to_the_diff_refs_head() {
    let json = format!(r#"{{"state":"opened","diff_refs":{{"head_sha":"{HEAD}"}}}}"#);
    assert_eq!(parse_view("gitlab", &json).unwrap().head_sha, HEAD);
}

#[test]
fn unreadable_or_surprising_output_is_unavailable_not_a_guess() {
    for bad in ["", "not json", "{}", r#"{"state":"LOCKED","headRefOid":"abc"}"#, r#"{"state":"OPEN"}"#, r#"{"state":"OPEN","headRefOid":"../x"}"#] {
        assert!(matches!(parse_view("github", bad), Err(ObservationError::Unavailable(_))), "{bad}");
    }
}

#[test]
fn authorization_failures_are_told_apart_from_outages() {
    for stderr in ["HTTP 403: Forbidden", "gh: HTTP 401: Bad credentials", "glab: 403 Forbidden", "To get started with GitHub CLI, please run: gh auth login", "Resource not accessible by personal access token"] {
        assert!(matches!(classify_failure(stderr), ObservationError::PermissionDenied(_)), "{stderr}");
    }
    for stderr in ["dial tcp: lookup api.github.com: no such host", "HTTP 502: Bad Gateway", "HTTP 404: Not Found", ""] {
        assert!(matches!(classify_failure(stderr), ObservationError::Unavailable(_)), "{stderr}");
    }
}

#[test]
fn failure_details_are_redacted_and_bounded() {
    let noisy = format!("token gh\x70_abcdefghijklmnopqrstuvwxyz0123456789 {}", "x".repeat(5000));
    let ObservationError::Unavailable(detail) = classify_failure(&noisy) else { panic!("expected unavailable") };
    assert!(!detail.contains("gh\x70_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(detail.len() <= 600, "{}", detail.len());
}
