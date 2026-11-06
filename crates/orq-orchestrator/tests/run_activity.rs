use std::time::Duration;

use orq_orchestrator::{newest_log_tail, run_activity, Event, LogTail, RunPhase};

fn event(kind: &str, stage: Option<&str>) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        kind: kind.to_string(),
        stage: stage.map(str::to_string),
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".to_string(),
        payload: serde_json::json!({}),
    }
}

fn tail(line: &str, quiet_secs: u64) -> LogTail {
    LogTail { size: 120, quiet_for: Duration::from_secs(quiet_secs), last_line: Some(line.to_string()) }
}

const STUCK_AFTER: Duration = Duration::from_secs(300);

#[test]
fn a_running_stage_shows_its_name_and_the_last_line_of_output() {
    let events = [event("stage.started", Some("verify"))];

    let activity = run_activity(&events, Some(&tail("running tests", 3)), STUCK_AFTER);

    assert_eq!(activity.phase, RunPhase::Running);
    assert_eq!(activity.stage.as_deref(), Some("verify"));
    assert_eq!(activity.last_output.as_deref(), Some("running tests"));
    assert!(!activity.may_be_stuck);
}

#[test]
fn a_running_stage_silent_for_too_long_may_be_stuck() {
    let events = [event("stage.started", Some("verify"))];

    let activity = run_activity(&events, Some(&tail("running tests", 301)), STUCK_AFTER);

    assert!(activity.may_be_stuck);
}

#[test]
fn a_finished_run_is_never_stuck_however_quiet() {
    let events = [event("stage.started", Some("verify")), event("worktree.removed", None)];

    let activity = run_activity(&events, Some(&tail("done", 9999)), STUCK_AFTER);

    assert!(!activity.may_be_stuck);
}

#[test]
fn secrets_in_the_last_line_are_redacted() {
    let events = [event("stage.started", Some("push"))];

    let activity = run_activity(&events, Some(&tail("token ghp_abcdefghijklmnopqrstuvwxyz0123456789", 1)), STUCK_AFTER);

    assert!(!activity.last_output.unwrap().contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
}

#[test]
fn the_newest_log_of_a_run_is_read_for_its_last_nonblank_line() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-activity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("old.log"), "ancient\n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(dir.join("stage.log"), "first\nsecond line\n\n  \n").unwrap();

    let tail = newest_log_tail(&dir).unwrap();

    assert_eq!(tail.last_line.as_deref(), Some("second line"));
    assert_eq!(tail.size, 22);
    assert!(tail.quiet_for < Duration::from_secs(60));
    assert!(newest_log_tail(&dir.join("missing")).is_none());
}
