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
    LogTail { size: 120, quiet_for: Duration::from_secs(quiet_secs), last_line: Some(line.to_string()), tests: None }
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
fn between_stages_the_run_is_never_stuck_and_names_no_finished_stage() {
    let events = [event("stage.started", Some("implement_and_review")), event("stage.completed", Some("implement_and_review"))];

    let activity = run_activity(&events, Some(&tail("old implement output", 9999)), STUCK_AFTER);

    assert_eq!(activity.phase, RunPhase::Running);
    assert!(!activity.may_be_stuck, "nothing runs between stages, so nothing can be stuck");
    assert_eq!(activity.stage, None, "a finished stage is not repeated");
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
fn the_tests_finished_in_the_newest_log_reach_the_activity_with_the_name_redacted() {
    let dir = log_dir("tests");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let mut log = format!("{}\nrunning 4 tests\n", orq_orchestrator::test_total_line(Some(4)));
    for n in 0..2 {
        log.push_str(&format!("test t{n} ... ok\n"));
    }
    log.push_str(&format!("test with::{token} ... ok\n"));
    std::fs::write(dir.join("gate.log"), log).unwrap();

    let tail = newest_log_tail(&dir).unwrap();
    let progress = tail.tests.clone().unwrap();
    assert_eq!((progress.done, progress.total), (3, 4));
    assert!(!progress.current.unwrap().contains("ghp_"));

    let events = [event("stage.started", Some("verify"))];
    let activity = run_activity(&events, Some(&tail), STUCK_AFTER);
    assert_eq!(activity.tests.map(|t| (t.done, t.total)), Some((3, 4)));
    assert_eq!(seen(&events, "x", 1).tests, None, "a log without tests has no progress");
}

#[test]
fn the_gate_and_ci_are_shown_as_a_step_with_its_round_and_percentage() {
    use orq_orchestrator::step_lines;
    let mut verify = event("stage.started", Some("verify"));
    verify.attempt = 2;
    let ci = {
        let mut e = event("stage.started", Some("ci"));
        e.attempt = 1;
        e
    };

    let testing = seen(&[verify.clone()], "x", 1);
    let with_tests = orq_orchestrator::RunActivity {
        tests: Some(orq_orchestrator::TestProgress { done: 1, total: 2, current: Some("a::one\u{1b}[31m".to_string()) }),
        ..testing.clone()
    };
    assert_eq!(step_lines(&with_tests), Some(vec!["Tests (Round 2) - 50%".to_string(), "  - a::one".to_string()]));
    assert_eq!(step_lines(&testing), Some(vec!["Checks (Round 2)".to_string()]));

    let waiting = seen(&[ci.clone()], "CI waiting: no checks reported for abc1234 yet (5s of 600s)", 1);
    assert_eq!(step_lines(&waiting), Some(vec!["CI pending".to_string()]));
    let running = seen(&[ci], "CI waiting: checks of abc1234 still running (5s of 600s)", 1);
    assert_eq!(step_lines(&running), Some(vec!["CI running (Round 1)".to_string()]));

    assert_eq!(step_lines(&seen(&[event("stage.started", Some("push"))], "x", 1)), None, "other stages keep their plain line");
}

#[test]
fn test_progress_carries_the_count_across_test_binaries_up_to_the_whole_suite() {
    use orq_orchestrator::{test_progress, test_total_line, TestProgress};
    let total = test_total_line(Some(60));

    let mut log = format!("{total}\n");
    let mut percentages = Vec::new();
    for (binary, tests) in [10, 20, 30].into_iter().enumerate() {
        log.push_str(&format!("\nrunning {tests} tests\n"));
        percentages.push(test_progress(&log).map(|t| t.done * 100 / t.total));
        for n in 0..tests {
            log.push_str(&format!("test b{binary}::t{n} ... ok\n"));
            percentages.push(test_progress(&log).map(|t| t.done * 100 / t.total));
        }
    }
    let percentages: Vec<u64> = percentages.into_iter().map(Option::unwrap).collect();
    assert!(percentages.windows(2).all(|pair| pair[0] <= pair[1]), "{percentages:?}");
    assert_eq!(percentages.iter().filter(|p| **p == 100).count(), 1, "only the last test reaches 100%: {percentages:?}");
    assert_eq!(percentages.last(), Some(&100));

    // The count carries over a new header; every kind of result counts.
    let two = format!("{total}\nrunning 2 tests\ntest x ... ok\ntest y ... FAILED\n\nrunning 10 tests\ntest z ... ignored\n");
    assert_eq!(test_progress(&two), Some(TestProgress { done: 3, total: 60, current: Some("z".to_string()) }));
    assert_eq!(test_progress(&format!("{total}\n")), Some(TestProgress { done: 0, total: 60, current: None }));
    // Never more than the total, however many lines look like results.
    let one = test_total_line(Some(1));
    assert_eq!(test_progress(&format!("{one}\ntest a ... ok\ntest b ... ok\n")).unwrap().done, 1);
}

#[test]
fn test_progress_has_no_percentage_without_a_known_total() {
    use orq_orchestrator::{test_progress, test_total_line};
    assert_eq!(test_progress("compiling\n"), None, "no test run has begun");
    assert_eq!(test_progress("running 4 tests\ntest a ... ok\n"), None, "one binary's header is not the suite");
    assert_eq!(test_progress(&format!("{}\n", test_total_line(Some(0)))), None, "nothing to measure");
    assert_eq!(test_progress(&format!("{}\nrunning 4 tests\ntest a ... ok\n", test_total_line(None))), None);

    // A later round of the gate in the same log starts from its own total, or from none.
    let round_one = format!("{}\nrunning 2 tests\ntest a ... ok\ntest b ... ok\n", test_total_line(Some(2)));
    let known = format!("{round_one}{}\nrunning 3 tests\ntest c ... ok\n", test_total_line(Some(3)));
    assert_eq!(test_progress(&known).map(|t| (t.done, t.total)), Some((1, 3)));
    let unknown = format!("{round_one}{}\nrunning 3 tests\ntest c ... ok\n", test_total_line(None));
    assert_eq!(test_progress(&unknown), None);
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

/// A worktree and store for one gate run, and a `cargo` on the returned PATH running `script`.
fn gate_with_cargo(tag: &str, script: &str) -> (orq_orchestrator::RunStore, std::path::PathBuf, Vec<(String, String)>) {
    use std::os::unix::fs::PermissionsExt;
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-activity-gate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (bin, logs, worktree) = (root.join("bin"), root.join("logs"), root.join("worktree"));
    for dir in [&bin, &logs, &worktree] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = orq_orchestrator::RunStore::new(&root.join("state"));
    store
        .create(&orq_orchestrator::RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree,
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    (store, logs, vec![("PATH".to_string(), path)])
}

#[test]
fn the_percentage_of_a_cargo_gate_covers_every_test_binary() {
    let script = r#"case "$*" in
metadata*) echo '{"packages":[]}' ;;
*--list*) printf 'a: test\n10 tests, 0 benchmarks\n20 tests, 0 benchmarks\n30 tests, 0 benchmarks\n' ;;
*) printf 'running 10 tests\n'; i=0; while [ $i -lt 10 ]; do echo "test a$i ... ok"; i=$((i+1)); done
   printf 'running 20 tests\ntest b ... ok\n' ;;
esac"#;
    let (store, logs, env) = gate_with_cargo("whole", script);
    let gate = orq_orchestrator::Gate::Steps { steps: vec![vec!["cargo".to_string(), "test".to_string(), "--workspace".to_string()]], notes: Vec::new() };

    let outcome = orq_orchestrator::run_gate_stage_with_env(&store, "run-1", &gate, true, &logs.join("gate.log"), &env).unwrap();

    assert_eq!(outcome, orq_orchestrator::GateOutcome::Passed);
    let tests = newest_log_tail(&logs).unwrap().tests.unwrap();
    assert_eq!((tests.done, tests.total), (11, 60));
}
