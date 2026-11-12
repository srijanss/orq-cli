use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use orq_core::review_lifecycle::ObservationError;
use orq_orchestrator::{observe_review, Event, RunRecord, RunStore};
use orq_test_support::FakeProvider;

const WAIT: Duration = Duration::from_secs(10);
const REPO: &str = "https://github.example/o/r";

fn world(tag: &str, provider: &str, repository: &str) -> RunStore {
    world_at(tag, provider, repository, "https://github.example/o/r.git")
}

fn world_at(tag: &str, provider: &str, repository: &str, remote_url: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-review-obs-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["remote", "add", "origin", remote_url])
        .output()
        .unwrap();
    assert!(out.status.success());
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
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

#[test]
fn a_provider_that_hangs_is_given_up_on_promptly() {
    let store = world("hang", "github", REPO);
    record_created(&store, "7");
    let provider = FakeProvider::builder()
        .reply_slowly(&["pr", "list"], "[]", 20_000)
        .build();
    let started = std::time::Instant::now();
    let outcome =
        observe_review(&store, "run-1", &provider.gh(), Duration::from_millis(400)).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(outcome, Err(ObservationError::Unavailable(ref d)) if d.contains("timed out")),
        "{outcome:?}"
    );
}

#[test]
fn a_missing_provider_program_is_unavailable() {
    let store = world("missing", "github", REPO);
    record_created(&store, "7");
    let nowhere = std::path::Path::new("/nonexistent/orq-test/gh");
    let outcome = observe_review(&store, "run-1", nowhere, WAIT).unwrap();
    assert!(
        matches!(outcome, Err(ObservationError::Unavailable(_))),
        "{outcome:?}"
    );
}

#[test]
fn oversized_stderr_is_bounded_and_still_classified() {
    let store = world("noisy", "github", REPO);
    record_created(&store, "7");
    let stderr = format!("HTTP 403: Forbidden\n{}", "noise ".repeat(100_000));
    let provider = FakeProvider::builder()
        .fail(&["pr", "list"], 1, &stderr)
        .build();
    let outcome = observe_review(&store, "run-1", &provider.gh(), WAIT).unwrap();
    match outcome {
        Err(ObservationError::PermissionDenied(detail)) => {
            assert!(detail.chars().count() <= 600, "{}", detail.len())
        }
        other => panic!("{other:?}"),
    }
}
