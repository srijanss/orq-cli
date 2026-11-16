//! A person's action on a run's review is recorded in the run's events, and the runs already merged
//! into their base locally are found from theirs, so their reviews can be resolved after the fact.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{local_merges, record_review_action, Event, RunRecord, RunStore};

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-review-events-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    RunStore::new(&dir)
}

fn record(run_id: &str) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "feat".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--feat".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--feat"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn event(run_id: &str, kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("{run_id}-{kind}"),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "feat".to_string(),
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-16T10:00:00Z".to_string(),
        payload,
    }
}

#[test]
fn a_review_action_is_recorded_with_the_head_and_who_took_it() {
    let store = store("action");
    store.start_run(&record("run-1")).unwrap();

    record_review_action(&store, "run-1", "review.dismissed", "aaaaaaa", "web").unwrap();

    let events = store.events("run-1").unwrap();
    let event = events.iter().find(|e| e.kind == "review.dismissed").expect("a review.dismissed event");
    assert_eq!(event.payload, serde_json::json!({ "head_sha": "aaaaaaa", "by": "web" }));
    assert_eq!(event.feature_id, "feat");
    assert!(record_review_action(&store, "run-9", "review.resolved", "aaaaaaa", "web").is_err(), "an unknown run has no events to add to");
}

#[test]
fn the_runs_merged_locally_are_found_with_the_commit_they_were_merged_at() {
    let store = store("merges");
    for run in ["run-1", "run-2", "run-3"] {
        store.start_run(&RunRecord { feature_id: format!("feat-{run}"), ..record(run) }).unwrap();
    }
    store.append_event(&event("run-1", "merge.completed", serde_json::json!({"head_sha": "1111111", "target_branch": "main"}))).unwrap();
    store.append_event(&event("run-3", "merge.completed", serde_json::json!({"target_branch": "main"}))).unwrap();

    assert_eq!(local_merges(&store).unwrap(), vec![("run-1".to_string(), "1111111".to_string())]);
}

#[test]
fn the_review_each_run_created_or_adopted_last_is_found_with_its_provider() {
    use orq_core::review_inbox::CreatedReview;
    let store = store("created");
    for run in ["run-1", "run-2", "run-3", "run-4"] {
        let provider = if run == "run-2" { "gitlab" } else { "github" };
        store.start_run(&RunRecord { feature_id: format!("feat-{run}"), provider: provider.into(), ..record(run) }).unwrap();
    }
    store.append_event(&event("run-1", "review.created", serde_json::json!({"id": "6", "url": "https://github.com/o/r/pull/6"}))).unwrap();
    store.append_event(&Event { event_id: "run-1-again".into(), ..event("run-1", "review.adopted", serde_json::json!({"id": "7", "url": "https://github.com/o/r/pull/7"})) }).unwrap();
    store.append_event(&event("run-2", "review.adopted", serde_json::json!({"id": "3", "url": "https://gitlab.com/o/r/-/merge_requests/3"}))).unwrap();
    store.append_event(&event("run-3", "review.created", serde_json::json!({"url": "https://github.com/o/r/pull/9"}))).unwrap();

    let found = orq_orchestrator::created_reviews(&store).unwrap();
    let review = |run: &str, provider: &str, id: &str, url: &str| CreatedReview { run_id: run.into(), provider: provider.into(), review_id: id.into(), url: url.into() };
    assert_eq!(
        found,
        vec![review("run-1", "github", "7", "https://github.com/o/r/pull/7"), review("run-2", "gitlab", "3", "https://gitlab.com/o/r/-/merge_requests/3")],
        "an event without an id, and a run without one, give nothing"
    );
}
