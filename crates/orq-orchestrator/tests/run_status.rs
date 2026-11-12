use orq_orchestrator::{summarize_run, Event, RunPhase};

fn event(kind: &str, stage: Option<&str>, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}-{}", stage.unwrap_or("none")),
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        kind: kind.to_string(),
        stage: stage.map(str::to_string),
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".to_string(),
        payload,
    }
}

fn none() -> serde_json::Value {
    serde_json::json!({})
}

#[test]
fn a_run_without_events_is_starting_with_no_stage() {
    let summary = summarize_run(&[]);

    assert_eq!(summary.phase, RunPhase::Running);
    assert_eq!(summary.stage, None);
    assert_eq!(summary.reason, None);
}

#[test]
fn a_started_stage_is_running_in_that_stage() {
    let events = [event("stage.started", Some("implement_and_review"), none())];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Running);
    assert_eq!(summary.stage.as_deref(), Some("implement_and_review"));
}

#[test]
fn a_completed_stage_keeps_the_run_running_until_the_next_one_starts() {
    let events = [
        event("stage.started", Some("push"), none()),
        event("stage.completed", Some("push"), none()),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Running);
    assert_eq!(summary.stage.as_deref(), Some("push"));
}

#[test]
fn a_failed_stage_fails_the_run_with_its_reason() {
    let events = [
        event("stage.started", Some("push"), none()),
        event(
            "stage.failed",
            Some("push"),
            serde_json::json!({ "reason": "git push of fix failed" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Failed);
    assert_eq!(summary.stage.as_deref(), Some("push"));
    assert_eq!(summary.reason.as_deref(), Some("git push of fix failed"));
}

#[test]
fn a_blocked_run_is_blocked_with_its_reason() {
    let events = [
        event("stage.started", Some("rebase"), none()),
        event(
            "run.blocked",
            Some("rebase"),
            serde_json::json!({ "reason": "conflicts" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Blocked);
    assert_eq!(summary.reason.as_deref(), Some("conflicts"));
}

#[test]
fn a_run_that_failed_after_a_stage_failure_reports_the_latest_reason() {
    let events = [
        event(
            "stage.failed",
            Some("verify"),
            serde_json::json!({ "reason": "tests failed" }),
        ),
        event(
            "run.failed",
            Some("verify"),
            serde_json::json!({ "reason": "gate fix budget spent" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Failed);
    assert_eq!(summary.reason.as_deref(), Some("gate fix budget spent"));
}

#[test]
fn a_cleaned_up_worktree_marks_the_run_completed() {
    let events = [
        event("stage.completed", Some("review"), none()),
        event(
            "worktree.removed",
            Some("cleanup"),
            serde_json::json!({ "worktree": "w" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Completed);
}

#[test]
fn a_kept_worktree_after_success_also_completes_the_run() {
    let events = [event(
        "worktree.kept",
        Some("cleanup"),
        serde_json::json!({ "reason": "dirty" }),
    )];

    assert_eq!(summarize_run(&events).phase, RunPhase::Completed);
}

#[test]
fn a_kept_worktree_does_not_hide_a_failed_run() {
    let events = [
        event(
            "stage.failed",
            Some("verify"),
            serde_json::json!({ "reason": "tests failed" }),
        ),
        event(
            "worktree.kept",
            Some("cleanup"),
            serde_json::json!({ "reason": "the run did not reach review_ready", "run_succeeded": false }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Failed);
    assert_eq!(summary.reason.as_deref(), Some("tests failed"));
}

#[test]
fn a_kept_worktree_does_not_hide_a_blocked_run_even_without_the_success_flag() {
    let events = [
        event(
            "run.blocked",
            Some("rebase"),
            serde_json::json!({ "reason": "conflicts" }),
        ),
        event(
            "worktree.kept",
            Some("cleanup"),
            serde_json::json!({ "reason": "x" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Blocked);
    assert_eq!(summary.reason.as_deref(), Some("conflicts"));
}

#[test]
fn a_kept_worktree_of_an_unsuccessful_run_does_not_complete_it() {
    let events = [
        event("stage.completed", Some("review"), none()),
        event(
            "worktree.kept",
            Some("cleanup"),
            serde_json::json!({ "reason": "CI did not pass", "run_succeeded": false }),
        ),
    ];

    assert_eq!(summarize_run(&events).phase, RunPhase::Running);
}

#[test]
fn a_skipped_stage_is_the_latest_stage_and_the_run_keeps_running() {
    let events = [
        event("stage.completed", Some("review"), none()),
        event(
            "stage.skipped",
            Some("ci"),
            serde_json::json!({ "reason": "disabled" }),
        ),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Running);
    assert_eq!(summary.stage.as_deref(), Some("ci"));
    assert_eq!(summary.reason, None);
}

#[test]
fn a_stage_that_starts_again_after_a_failure_is_running_again() {
    let events = [
        event(
            "stage.failed",
            Some("verify"),
            serde_json::json!({ "reason": "tests failed" }),
        ),
        event("stage.started", Some("verify"), none()),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Running);
    assert_eq!(summary.reason, None);
}

#[test]
fn a_failure_without_a_reason_has_none_and_unknown_events_are_ignored() {
    let events = [
        event("stage.started", Some("ci"), none()),
        event("something.new", None, none()),
        event("stage.failed", Some("ci"), none()),
        event("another.unknown", None, none()),
    ];

    let summary = summarize_run(&events);

    assert_eq!(summary.phase, RunPhase::Failed);
    assert_eq!(summary.stage.as_deref(), Some("ci"));
    assert_eq!(summary.reason, None);
}

#[test]
fn a_run_completed_event_ends_the_run_with_the_ending_it_names() {
    let started = event("stage.started", Some("push"), none());

    let done = summarize_run(&[
        started.clone(),
        event(
            "run.completed",
            None,
            serde_json::json!({ "ending": "review_ready" }),
        ),
    ]);
    assert_eq!(done.phase, RunPhase::Completed);
    assert_eq!(done.reason, None);

    let failed = summarize_run(&[
        started.clone(),
        event(
            "run.completed",
            None,
            serde_json::json!({ "ending": "failed", "reason": "no harness" }),
        ),
    ]);
    assert_eq!(failed.phase, RunPhase::Failed);
    assert_eq!(failed.reason.as_deref(), Some("no harness"));

    let blocked = summarize_run(&[
        started,
        event(
            "run.completed",
            None,
            serde_json::json!({ "ending": "blocked", "reason": "no commit" }),
        ),
    ]);
    assert_eq!(blocked.phase, RunPhase::Blocked);
    assert_eq!(blocked.reason.as_deref(), Some("no commit"));
}

#[test]
fn a_run_completed_event_wins_over_the_events_before_it_but_an_unknown_ending_is_ignored() {
    let events = [
        event(
            "stage.failed",
            Some("ci"),
            serde_json::json!({ "reason": "red" }),
        ),
        event(
            "worktree.kept",
            Some("cleanup"),
            serde_json::json!({ "run_succeeded": false }),
        ),
        event(
            "run.completed",
            None,
            serde_json::json!({ "ending": "failed", "reason": "ci was red" }),
        ),
    ];
    let summary = summarize_run(&events);
    assert_eq!(summary.phase, RunPhase::Failed);
    assert_eq!(summary.reason.as_deref(), Some("ci was red"));

    let odd = summarize_run(&[
        event("stage.started", Some("push"), none()),
        event(
            "run.completed",
            None,
            serde_json::json!({ "ending": "something_new" }),
        ),
    ]);
    assert_eq!(odd.phase, RunPhase::Running);
}

#[test]
fn a_run_that_ended_pushed_is_completed_even_with_its_worktree_still_running_a_stage() {
    let events = [
        event("stage.started", Some("push"), none()),
        event("run.completed", None, serde_json::json!({"ending": "pushed"})),
    ];

    assert_eq!(summarize_run(&events).phase, RunPhase::Completed);
}

#[test]
fn a_run_that_ended_merged_is_completed() {
    let events = [
        event("stage.started", Some("gate"), none()),
        event("run.completed", None, serde_json::json!({"ending": "merged"})),
    ];

    assert_eq!(summarize_run(&events).phase, RunPhase::Completed);
}
