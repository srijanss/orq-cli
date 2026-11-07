use std::time::Duration;

use orq_orchestrator::{activity_line, newest_log_tail, run_activity, Event, LogTail, RunPhase};

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

fn log_dir(tag: &str) -> std::path::PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-activity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_secret_cut_by_the_display_length_is_redacted_before_it_is_cut() {
    let dir = log_dir("cut");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    std::fs::write(dir.join("stage.log"), format!("{} {token}\n", "x".repeat(190))).unwrap();

    let line = newest_log_tail(&dir).unwrap().last_line.unwrap();

    assert!(!line.contains("ghp_"), "{line}");
}

#[test]
fn what_a_tail_cannot_vouch_for_is_never_shown() {
    use std::os::unix::fs::symlink;
    let dir = log_dir("edges");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    // The tail window (8 KiB) opens in the middle of the token's line, then one whole line follows.
    let long = format!("{}{token}\n", "y".repeat(8 * 1024 - 20));
    std::fs::write(dir.join("stage.log"), format!("{long}last whole line\n")).unwrap();
    assert_eq!(newest_log_tail(&dir).unwrap().last_line.as_deref(), Some("last whole line"));

    // A tail that is only the middle of one line shows nothing rather than a fragment.
    std::fs::write(dir.join("stage.log"), format!("{}\n", "z".repeat(20 * 1024))).unwrap();
    assert_eq!(newest_log_tail(&dir).unwrap().last_line, None);

    // Blank-only, directories named like logs, and links are not output.
    std::fs::write(dir.join("stage.log"), "\n  \n").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::create_dir(dir.join("newer.log")).unwrap();
    std::fs::write(dir.join("secret.txt"), "from elsewhere\n").unwrap();
    symlink(dir.join("secret.txt"), dir.join("link.log")).unwrap();
    let tail = newest_log_tail(&dir).unwrap();
    assert_eq!(tail.last_line, None);
    assert_eq!(tail.size, 4);
}

#[test]
fn a_token_cut_by_the_start_of_the_tail_is_not_shown() {
    let dir = log_dir("straddle");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    // One unterminated line: the tail window opens inside the token and nothing whole follows.
    std::fs::write(dir.join("stage.log"), format!("{}{token}{}", "p".repeat(100), "q".repeat(8 * 1024 - 20))).unwrap();
    assert_eq!(newest_log_tail(&dir).unwrap().last_line, None);

    // The window opening inside a multi-byte character is no different.
    std::fs::write(dir.join("stage.log"), format!("{}x", "é".repeat(8 * 1024))).unwrap();
    assert_eq!(newest_log_tail(&dir).unwrap().last_line, None);
}

fn seen(events: &[Event], line: &str, quiet: u64) -> orq_orchestrator::RunActivity {
    run_activity(events, Some(&tail(line, quiet)), STUCK_AFTER)
}

#[test]
fn the_round_of_the_latest_stage_event_is_carried_on_the_activity() {
    let mut first = event("stage.started", Some("verify"));
    first.attempt = 1;
    let mut second = event("stage.started", Some("verify"));
    second.attempt = 2;

    assert_eq!(seen(&[first.clone()], "running", 1).attempt, 1);
    assert_eq!(seen(&[first, second], "running", 1).attempt, 2);
    assert_eq!(seen(&[], "running", 1).attempt, 0, "no stage has started yet");
}

#[test]
fn test_progress_counts_the_tests_finished_since_the_latest_run_began() {
    use orq_orchestrator::{test_progress, TestProgress};
    let log = "running 4 tests\ntest a::one ... ok\ntest a::two ... FAILED\ntest a::three ... ignored\n";
    assert_eq!(
        test_progress(log),
        Some(TestProgress { done: 3, total: 4, current: Some("a::three".to_string()) })
    );

    // A later test binary starts the count again.
    let two_binaries = "running 2 tests\ntest x ... ok\ntest y ... ok\n\nrunning 10 tests\ntest z ... ok\n";
    assert_eq!(
        test_progress(two_binaries),
        Some(TestProgress { done: 1, total: 10, current: Some("z".to_string()) })
    );

    assert_eq!(test_progress("running 3 tests\n").unwrap().current, None);
    assert_eq!(test_progress("compiling\n"), None, "no test run has begun");
    assert_eq!(test_progress("running 0 tests\n"), None, "nothing to measure");
    // Never more than the total, however many lines look like results.
    assert_eq!(test_progress("running 1 tests\ntest a ... ok\ntest b ... ok\n").unwrap().done, 1);
}

#[test]
fn a_line_is_shown_when_the_stage_or_output_changes_and_not_otherwise() {
    let verify = [event("stage.started", Some("verify"))];
    let push = [event("stage.started", Some("push"))];
    let none = Duration::ZERO;
    let heartbeat = Duration::from_secs(60);

    let first = activity_line(None, &seen(&verify, "running tests", 1), none, heartbeat).unwrap();
    assert!(first.contains("verify") && first.contains("running tests"), "{first}");

    let same = seen(&verify, "running tests", 2);
    assert_eq!(activity_line(Some(&seen(&verify, "running tests", 1)), &same, Duration::from_secs(5), heartbeat), None);

    let more = seen(&verify, "42 passed", 1);
    assert!(activity_line(Some(&same), &more, Duration::from_secs(5), heartbeat).unwrap().contains("42 passed"));

    let next = seen(&push, "42 passed", 1);
    assert!(activity_line(Some(&more), &next, Duration::from_secs(5), heartbeat).unwrap().contains("push"));
}

#[test]
fn a_quiet_stage_says_it_is_still_running_once_the_heartbeat_has_passed() {
    let verify = [event("stage.started", Some("verify"))];
    let quiet = seen(&verify, "running tests", 130);
    let heartbeat = Duration::from_secs(60);

    let line = activity_line(Some(&quiet), &quiet, Duration::from_secs(61), heartbeat).unwrap();

    assert!(line.contains("verify") && line.contains("still running") && line.contains("130"), "{line}");
    assert_eq!(activity_line(Some(&quiet), &quiet, Duration::from_secs(59), heartbeat), None);
}

#[test]
fn control_sequences_in_output_never_reach_the_line_shown_on_a_terminal() {
    let verify = [event("stage.started", Some("verify"))];
    let hostile = "ok\u{1b}[2J\u{1b}]52;c;aGk=\u{7}\rfake: all passed\u{8}\u{8}";

    let line = activity_line(None, &seen(&verify, hostile, 1), Duration::ZERO, Duration::from_secs(60)).unwrap();

    assert!(!line.chars().any(char::is_control), "{line:?}");
    assert!(line.contains("ok") && line.contains("fake: all passed"), "{line:?}");
}
