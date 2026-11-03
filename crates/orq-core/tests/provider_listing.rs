use orq_core::no_merge::forbidden_provider_call;
use orq_core::provider_observation::{list_command, parse_list, view_command, LIST_PAGE};
use orq_core::review_lifecycle::{ObservationError, ProviderState};

const HEAD_A: &str = "1111111111111111111111111111111111111111";
const HEAD_B: &str = "2222222222222222222222222222222222222222";
const REPO: &str = "https://github.example/o/r";

#[test]
fn github_listing_asks_for_every_state_on_the_branch_pair() {
    let (program, args) = list_command("github", REPO, "fix--COM-02", "main").unwrap();
    assert_eq!(program, "gh");
    let joined = args.join(" ");
    for part in ["pr list", "--repo https://github.example/o/r", "--head fix--COM-02", "--base main", "--state all", "headRefOid", "isCrossRepository"] {
        assert!(joined.contains(part), "{part} in {joined}");
    }
    assert!(forbidden_provider_call(&program, &args).is_none());
}

#[test]
fn gitlab_listing_asks_for_every_state_on_the_branch_pair() {
    let (program, args) = list_command("gitlab", "https://gitlab.example/o/r", "fix--COM-02", "main").unwrap();
    assert_eq!(program, "glab");
    let joined = args.join(" ");
    for part in ["mr list", "--source-branch fix--COM-02", "--target-branch main", "--all", "--output json"] {
        assert!(joined.contains(part), "{part} in {joined}");
    }
    assert!(forbidden_provider_call(&program, &args).is_none());
}

#[test]
fn the_pinned_https_repository_is_accepted_but_credentials_and_flags_are_not() {
    assert!(view_command("github", REPO, "7").is_ok());
    for bad in ["https://user:pw@github.example/o/r", "https://github.example/o/r?x=1", "https://github.example/o/r#f", "--hostname=evil", "file:///etc", "https://a b/c"] {
        assert!(view_command("github", bad, "7").is_err(), "{bad}");
        assert!(list_command("github", bad, "b", "main").is_err(), "{bad}");
    }
    assert!(list_command("github", REPO, "--web", "main").is_err());
    assert!(list_command("github", REPO, "b", "-x").is_err());
}

#[test]
fn github_rows_become_candidates_with_their_head_commits() {
    let json = format!(
        r#"[{{"number":7,"url":"https://github.example/o/r/pull/7","state":"OPEN","headRefOid":"{HEAD_A}","isCrossRepository":false}},
            {{"number":9,"url":"https://github.example/o/r/pull/9","state":"MERGED","headRefOid":"{HEAD_B}","isCrossRepository":false}}]"#
    );
    let listed = parse_list("github", &json).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].candidate.review_id, "7");
    assert_eq!(listed[0].candidate.state, ProviderState::Open);
    assert_eq!(listed[0].head_sha, HEAD_A);
    assert_eq!(listed[1].candidate.state, ProviderState::Merged);
}

#[test]
fn gitlab_rows_use_iid_web_url_and_treat_locked_as_closed() {
    let json = format!(
        r#"[{{"iid":3,"web_url":"https://gitlab.example/o/r/-/merge_requests/3","state":"opened","sha":"{HEAD_A}","source_project_id":5,"target_project_id":5}},
            {{"iid":4,"web_url":"https://gitlab.example/o/r/-/merge_requests/4","state":"locked","sha":"{HEAD_B}","source_project_id":5,"target_project_id":5}}]"#
    );
    let listed = parse_list("gitlab", &json).unwrap();
    assert_eq!(listed[0].candidate.review_id, "3");
    assert_eq!(listed[0].candidate.state, ProviderState::Open);
    assert_eq!(listed[1].candidate.state, ProviderState::Closed);
}

#[test]
fn reviews_from_forks_do_not_review_this_branch() {
    let gh = format!(r#"[{{"number":7,"url":"https://h/o/r/pull/7","state":"OPEN","headRefOid":"{HEAD_A}","isCrossRepository":true}}]"#);
    assert!(parse_list("github", &gh).unwrap().is_empty());
    let gl = format!(r#"[{{"iid":3,"web_url":"https://h/o/r/-/merge_requests/3","state":"opened","sha":"{HEAD_A}","source_project_id":6,"target_project_id":5}}]"#);
    assert!(parse_list("gitlab", &gl).unwrap().is_empty());
}

#[test]
fn an_answer_that_cannot_be_trusted_is_unavailable() {
    let full: Vec<String> = (1..=LIST_PAGE)
        .map(|n| format!(r#"{{"number":{n},"url":"https://h/o/r/pull/{n}","state":"OPEN","headRefOid":"{HEAD_A}","isCrossRepository":false}}"#))
        .collect();
    let full_page = format!("[{}]", full.join(","));
    let cases = [
        full_page.as_str(),
        "not json",
        "{}",
        r#"[{"number":7,"url":"https://h/o/r/pull/7","state":"OPEN","headRefOid":"abc"}]"#,
        r#"[{"number":7,"url":"https://h/o/r/pull/7","state":"WEIRD","headRefOid":"1111111","isCrossRepository":false}]"#,
        r#"[{"number":"x","url":"https://h/o/r/pull/7","state":"OPEN","headRefOid":"1111111","isCrossRepository":false}]"#,
        r#"[{"number":7,"url":"javascript:alert(1)","state":"OPEN","headRefOid":"1111111","isCrossRepository":false}]"#,
    ];
    for bad in cases {
        assert!(matches!(parse_list("github", bad), Err(ObservationError::Unavailable(_))), "{}", &bad[..bad.len().min(80)]);
    }
}

#[test]
fn an_empty_listing_means_no_review_yet() {
    assert!(parse_list("github", "[]").unwrap().is_empty());
}
