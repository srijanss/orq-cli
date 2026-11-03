use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use orq_core::review_lifecycle::{ObservationError, ProviderState};
use orq_orchestrator::{observe_review, Event, RunRecord, RunStore, StoreError};
use orq_test_support::FakeProvider;

const WAIT: Duration = Duration::from_secs(10);
const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const REPO: &str = "https://github.example/o/r";

fn world(tag: &str, provider: &str, repository: &str) -> RunStore {
    world_at(tag, provider, repository, "https://github.example/o/r.git")
}

fn world_at(tag: &str, provider: &str, repository: &str, remote_url: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!("orq-review-obs-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let out = std::process::Command::new("git").arg("-C").arg(&worktree).args(["init", "-q", "-b", "main"]).output().unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("git").arg("-C").arg(&worktree).args(["remote", "add", "origin", remote_url]).output().unwrap();
    assert!(out.status.success());
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: provider.to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree,
            repository: repository.to_string(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store
}

fn record_created(store: &RunStore, id: &str) {
    let record = store.load("run-1").unwrap();
    store
        .append_event(
            &Event {
                schema_version: 1,
                event_id: "e-created".to_string(),
                run_id: record.run_id,
                project_id: record.project_id,
                feature_id: record.feature_id,
                kind: "review.created".to_string(),
                stage: Some("review_request".to_string()),
                attempt: 1,
                occurred_at: "2026-01-01T00:00:00Z".to_string(),
                payload: serde_json::json!({ "url": format!("https://github.example/o/r/pull/{id}"), "id": id, "provider": "github" }),
            },
        )
        .unwrap();
}

fn row(number: u32, state: &str, head: &str) -> String {
    format!(r#"{{"number":{number},"url":"https://github.example/o/r/pull/{number}","state":"{state}","headRefOid":"{head}","isCrossRepository":false}}"#)
}

fn listing(rows: &[String]) -> String {
    format!("[{}]\n", rows.join(","))
}

#[test]
fn an_open_review_is_observed_with_its_current_head() {
    let store = world("open", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[row(7, "OPEN", NEW)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_eq!(seen.observation.state, ProviderState::Open);
    assert_eq!(seen.observation.head_sha, NEW);
    assert_eq!(seen.review.review_id, "7");
    assert!(seen.duplicates.is_empty());
    let calls = provider.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.iter().any(|a| a == "https://github.example/o/r.git") && calls[0].args.iter().any(|a| a == "fix--COM-02"));
}

#[test]
fn a_review_merged_by_a_person_is_observed_as_merged_and_orq_only_reads() {
    let store = world("merged", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[row(7, "MERGED", OLD)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_eq!(seen.observation.state, ProviderState::Merged);
    for call in provider.calls() {
        let verbs: Vec<&str> = call.args.iter().map(String::as_str).collect();
        assert!(!verbs.contains(&"merge") && !verbs.contains(&"approve") && !verbs.contains(&"create") && !verbs.contains(&"close"), "{verbs:?}");
    }
}

#[test]
fn a_closed_unmerged_review_is_observed_as_closed() {
    let store = world("closed", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[row(7, "CLOSED", OLD)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_eq!(seen.observation.state, ProviderState::Closed);
}

#[test]
fn duplicates_are_reconciled_to_the_recorded_review() {
    let store = world("dups", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[row(9, "OPEN", NEW), row(7, "OPEN", OLD)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_eq!(seen.review.review_id, "7");
    assert_eq!(seen.observation.head_sha, OLD);
    assert_eq!(seen.duplicates.len(), 1);
    assert_eq!(seen.duplicates[0].review_id, "9");
}

#[test]
fn a_force_pushed_head_is_reported_as_the_provider_now_has_it() {
    let store = world("force", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[row(7, "OPEN", NEW)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_ne!(seen.observation.head_sha, OLD);
}

#[test]
fn a_gitlab_run_uses_glab() {
    let store = world_at("gitlab", "gitlab", "https://gitlab.example/o/r", "https://gitlab.example/o/r.git");
    let rows = format!(r#"[{{"iid":3,"web_url":"https://gitlab.example/o/r/-/merge_requests/3","state":"opened","sha":"{NEW}","source_project_id":5,"target_project_id":5}}]"#);
    let provider = FakeProvider::builder().reply(&["mr", "list"], &rows).build();
    let seen = observe_review(&store, "run-1", &provider.glab(), WAIT).unwrap().unwrap();
    assert_eq!(seen.review.review_id, "3");
    assert_eq!(provider.calls()[0].program, "glab");
}

#[test]
fn a_permission_error_is_told_apart_from_an_outage() {
    let store = world("denied", "github", REPO);
    let provider = FakeProvider::builder().fail(&["pr", "list"], 1, "HTTP 403: Forbidden\n").build();
    let denied = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap_err();
    assert!(matches!(denied, ObservationError::PermissionDenied(_)), "{denied:?}");

    let down = FakeProvider::builder().fail(&["pr", "list"], 1, "HTTP 502: Bad Gateway\n").build();
    let outage = observe_review(&store, "run-1", &down.gh(), WAIT).unwrap().unwrap_err();
    assert!(matches!(outage, ObservationError::Unavailable(_)), "{outage:?}");
}

#[test]
fn a_provider_that_finds_no_review_or_garbage_is_unavailable() {
    let store = world("none", "github", REPO);
    let empty = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    assert!(matches!(observe_review(&store, "run-1", &empty.gh(), WAIT).unwrap(), Err(ObservationError::Unavailable(_))));
    let garbage = FakeProvider::builder().reply(&["pr", "list"], "<html>oops</html>").build();
    assert!(matches!(observe_review(&store, "run-1", &garbage.gh(), WAIT).unwrap(), Err(ObservationError::Unavailable(_))));
}

#[test]
fn a_repointed_remote_or_an_unknown_provider_is_a_local_error() {
    let repointed = world_at("repointed", "github", REPO, "https://evil.example/o/r.git");
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]\n").build();
    assert!(matches!(observe_review(&repointed, "run-1", &provider.gh(), WAIT), Err(StoreError::Corrupt(_))));
    assert!(provider.calls().is_empty(), "nothing is asked of the provider about an unverified repository");

    let odd = world("odd", "bitbucket", REPO);
    assert!(observe_review(&odd, "run-1", &provider.gh(), WAIT).is_err());
    assert!(observe_review(&odd, "no-such-run", &provider.gh(), WAIT).is_err());
}

fn padded_row(number: u32, state: &str, head: &str, padding: usize) -> String {
    let url = format!("https://github.example/o/r/pull/{number}");
    let pad = "x".repeat(padding);
    format!(r#"{{"number":{number},"url":"{url}","state":"{state}","headRefOid":"{head}","isCrossRepository":false,"body":"{pad}"}}"#)
}

#[test]
fn a_valid_listing_larger_than_the_draft_limit_is_read_whole() {
    let store = world("big", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[padded_row(7, "OPEN", NEW, 200 * 1024)])).build();
    let seen = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap().unwrap();
    assert_eq!(seen.review.review_id, "7");
    assert_eq!(seen.observation.head_sha, NEW);
}

#[test]
fn a_listing_beyond_the_observation_budget_is_unavailable_not_truncated() {
    let store = world("huge", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing(&[padded_row(7, "OPEN", NEW, 6 * 1024 * 1024)])).build();
    let outcome = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap();
    match outcome {
        Err(ObservationError::Unavailable(detail)) => assert!(detail.contains("larger"), "{detail}"),
        other => panic!("{other:?}"),
    }
}
