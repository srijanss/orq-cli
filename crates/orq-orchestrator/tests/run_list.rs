use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{
    list_runs, list_runs_report, Event, RoleProfile, RunPhase, RunRecord, RunStore,
};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-run-list-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    RunStore::new(&root)
}

fn profile(model: &str) -> RoleProfile {
    RoleProfile {
        harness: "pi".to_string(),
        model: model.to_string(),
        thinking: None,
    }
}

fn record(run_id: &str, project: &str, feature: &str) -> RunRecord {
    RunRecord {
        run_id: run_id.to_string(),
        project_id: project.to_string(),
        feature_id: feature.to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "abc".to_string(),
        branch: format!("fix--{feature}"),
        worktree: PathBuf::from("/tmp/w"),
        repository: String::new(),
        profiles: BTreeMap::from([
            ("implementer".to_string(), profile("gpt-6")),
            ("reviewer".to_string(), profile("opus")),
        ]),
    }
}

fn event(r: &RunRecord, kind: &str, stage: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{}-{kind}-{stage}", r.run_id),
        run_id: r.run_id.clone(),
        project_id: r.project_id.clone(),
        feature_id: r.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(stage.to_string()),
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".to_string(),
        payload,
    }
}

#[test]
fn only_the_runs_of_the_project_are_listed_with_feature_models_and_status() {
    let s = store("project");
    let mine = record("run-a", "repo-1", "COM-02");
    let other = record("run-b", "repo-2", "COM-09");
    s.create(&mine).unwrap();
    s.create(&other).unwrap();
    s.append_event(&event(
        &mine,
        "stage.started",
        "implement_and_review",
        serde_json::json!({}),
    ))
    .unwrap();
    s.append_event(&event(
        &other,
        "stage.failed",
        "push",
        serde_json::json!({ "reason": "x" }),
    ))
    .unwrap();

    let runs = list_runs(&s, "repo-1").unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, "run-a");
    assert_eq!(runs[0].feature_id, "COM-02");
    assert_eq!(runs[0].phase, RunPhase::Running);
    assert_eq!(runs[0].stage.as_deref(), Some("implement_and_review"));
    assert_eq!(runs[0].implementer_model.as_deref(), Some("gpt-6"));
    assert_eq!(runs[0].reviewer_model.as_deref(), Some("opus"));
    assert_eq!(runs[0].review_url, None);
}

#[test]
fn a_run_with_a_created_review_lists_its_link_and_a_failed_run_its_reason() {
    let s = store("review");
    let done = record("run-a", "repo-1", "COM-02");
    let failed = record("run-b", "repo-1", "COM-03");
    s.create(&done).unwrap();
    s.create(&failed).unwrap();
    s.append_event(&event(
        &done,
        "review.created",
        "create_review",
        serde_json::json!({ "url": "https://x/pull/1", "id": "1" }),
    ))
    .unwrap();
    s.append_event(&event(
        &failed,
        "stage.failed",
        "verify",
        serde_json::json!({ "reason": "tests failed" }),
    ))
    .unwrap();

    let runs = list_runs(&s, "repo-1").unwrap();

    assert_eq!(
        runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
        ["run-a", "run-b"]
    );
    assert_eq!(runs[0].review_url.as_deref(), Some("https://x/pull/1"));
    assert_eq!(runs[1].phase, RunPhase::Failed);
    assert_eq!(runs[1].reason.as_deref(), Some("tests failed"));
}

#[test]
fn a_store_without_runs_lists_nothing() {
    let s = store("empty");

    assert!(list_runs(&s, "repo-1").unwrap().is_empty());
}

fn runs_dir(root_tag: &str) -> PathBuf {
    orq_test_support::process_temp_dir()
        .join(format!("orq-run-list-{}-{root_tag}", std::process::id()))
        .join("runs")
}

#[test]
fn a_stray_file_or_non_run_directory_under_runs_does_not_break_the_listing() {
    let s = store("stray");
    s.create(&record("run-a", "repo-1", "COM-02")).unwrap();
    let runs = runs_dir("stray");
    std::fs::write(runs.join("notes"), "not a run").unwrap();
    std::fs::create_dir(runs.join("empty-dir")).unwrap();
    std::fs::create_dir(runs.join(".hidden")).unwrap();

    let listed = list_runs(&s, "repo-1").unwrap();

    assert_eq!(
        listed.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
        ["run-a"]
    );
}

#[test]
fn a_corrupt_record_or_event_log_is_skipped_and_counted_while_the_other_runs_still_list() {
    let s = store("corrupt");
    s.create(&record("run-a", "repo-1", "COM-02")).unwrap();
    s.create(&record("run-b", "repo-1", "COM-03")).unwrap();
    s.create(&record("run-c", "repo-1", "COM-04")).unwrap();
    std::fs::write(
        runs_dir("corrupt").join("run-a").join("events.jsonl"),
        "not json\n",
    )
    .unwrap();
    std::fs::write(runs_dir("corrupt").join("run-b").join("run.json"), "{").unwrap();

    let report = list_runs_report(&s, "repo-1").unwrap();
    assert_eq!(
        report
            .runs
            .iter()
            .map(|r| r.run_id.as_str())
            .collect::<Vec<_>>(),
        ["run-c"]
    );
    assert_eq!(report.unreadable, 2);
    assert_eq!(list_runs(&s, "repo-1").unwrap().len(), 1);

    // Another project's broken log is not even opened, so it is not counted.
    let other = list_runs_report(&s, "repo-2").unwrap();
    assert!(other.runs.is_empty());
    assert_eq!(
        other.unreadable, 1,
        "only the record that cannot be attributed to a project is counted"
    );
}

#[test]
fn an_adopted_review_lists_its_link_and_a_run_without_profiles_lists_no_models() {
    let s = store("adopted");
    let mut r = record("run-a", "repo-1", "COM-02");
    r.profiles.clear();
    s.create(&r).unwrap();
    s.append_event(&event(
        &r,
        "review.adopted",
        "create_review",
        serde_json::json!({ "url": "https://x/pull/7", "id": "7" }),
    ))
    .unwrap();

    let runs = list_runs(&s, "repo-1").unwrap();

    assert_eq!(runs[0].review_url.as_deref(), Some("https://x/pull/7"));
    assert_eq!(runs[0].implementer_model, None);
    assert_eq!(runs[0].reviewer_model, None);
}
