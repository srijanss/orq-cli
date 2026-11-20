use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{
    run_implement_stage, ImplementSession, RoleProfile, RunRecord, RunStore, StoreError,
};

fn root(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-implement-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(run_id: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile {
            harness: "claude".to_string(),
            model: "opus".to_string(),
            thinking: None,
        },
    );
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "git@github.com:acme/app.git".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--COM-02".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--COM-02"),
        repository: String::new(),
        profiles,
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

struct Setup {
    store: RunStore,
    cwd: PathBuf,
    log: PathBuf,
}

fn setup(tag: &str) -> Setup {
    let root = root(tag);
    let store = RunStore::new(&root.join("state"));
    store.create(&record("run-1")).unwrap();
    let cwd = root.join("worktree");
    fs::create_dir_all(&cwd).unwrap();
    Setup {
        store,
        cwd,
        log: root.join("implement.log"),
    }
}

fn session<'a>(
    s: &'a Setup,
    args: &'a [String],
    env: &'a [(String, String)],
) -> ImplementSession<'a> {
    ImplementSession {
        program: "sh",
        args,
        env,
        cwd: &s.cwd,
        session_id: Some("sess-42"),
        log: &s.log,
        terminal: false,
        exit_marker: None,
    }
}

fn kinds(store: &RunStore) -> Vec<String> {
    store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn a_finished_session_records_start_and_completion_with_its_session_log_and_exit_code() {
    let s = setup("ok");
    let args = argv(&["-c", r#"pwd; echo out-""ok; echo err-""ok >&2"#]);

    let outcome = run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert!(outcome.success);
    assert_eq!(outcome.exit_code, Some(0));
    let events = s.store.events("run-1").unwrap();
    assert_eq!(
        events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["stage.started", "stage.completed"]
    );
    for event in &events {
        assert_eq!(event.stage.as_deref(), Some("implement_and_review"));
        assert_eq!(event.attempt, 1);
        assert_eq!(
            (
                event.run_id.as_str(),
                event.project_id.as_str(),
                event.feature_id.as_str()
            ),
            ("run-1", "repo-1", "COM-02")
        );
        assert_eq!(event.payload["session_id"], "sess-42");
        assert_eq!(event.payload["log"], s.log.to_str().unwrap());
    }
    assert_ne!(events[0].event_id, events[1].event_id);
    assert_eq!(events[1].payload["exit_code"], 0);

    // Run in the worktree, with both streams captured in the log.
    let log = fs::read_to_string(&s.log).unwrap();
    assert!(
        log.contains(fs::canonicalize(&s.cwd).unwrap().to_str().unwrap()),
        "{log}"
    );
    assert!(
        log.contains("out-ok\n") && log.contains("err-ok\n"),
        "{log}"
    );
    // The session's own output only: orq writes its lines only for a session on the terminal.
    assert!(!log.contains("[orq "), "{log}");
}

#[cfg(unix)]
#[test]
fn the_session_log_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("private-log");
    let args = argv(&["-c", "exit 0"]);

    run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    let mode = fs::metadata(&s.log).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "log mode is {mode:o}");
}

#[cfg(unix)]
#[test]
fn an_existing_open_log_is_tightened_and_keeps_its_contents() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("existing-log");
    fs::write(&s.log, "earlier\n").unwrap();
    fs::set_permissions(&s.log, fs::Permissions::from_mode(0o644)).unwrap();
    let args = argv(&["-c", "echo later"]);

    run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert_eq!(
        fs::metadata(&s.log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_to_string(&s.log).unwrap(), "earlier\nlater\n");
}

#[test]
fn a_failing_session_is_recorded_as_failed_with_its_exit_code() {
    let s = setup("fail");
    let args = argv(&["-c", "exit 5"]);

    let outcome = run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, Some(5));
    assert_eq!(kinds(&s.store), ["stage.started", "stage.failed"]);
    let failed = &s.store.events("run-1").unwrap()[1];
    assert_eq!(failed.payload["exit_code"], 5);
    assert!(failed.payload["reason"].as_str().unwrap().contains('5'));
}

#[cfg(unix)]
#[test]
fn a_session_killed_by_a_signal_is_failed_without_an_exit_code_and_says_so() {
    let s = setup("signal");
    let args = argv(&["-c", "kill -TERM $$"]);

    let outcome = run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, None);
    assert_eq!(kinds(&s.store), ["stage.started", "stage.failed"]);
    let failed = &s.store.events("run-1").unwrap()[1];
    assert_eq!(failed.payload["exit_code"], serde_json::Value::Null);
    assert_eq!(failed.payload["signal"], 15);
    // Only SIGKILL points at the out-of-memory killer.
    assert_eq!(
        failed.payload["reason"],
        "sh was stopped by signal 15 (SIGTERM)"
    );
}

#[test]
fn a_program_that_cannot_start_is_a_failed_stage_without_an_exit_code() {
    let s = setup("missing");
    let args = argv(&[]);
    let mut request = session(&s, &args, &[]);
    request.program = "orq-no-such-harness";

    let outcome = run_implement_stage(&s.store, "run-1", &request).unwrap();

    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, None);
    assert_eq!(kinds(&s.store), ["stage.started", "stage.failed"]);
    let failed = &s.store.events("run-1").unwrap()[1];
    assert_eq!(failed.payload["exit_code"], serde_json::Value::Null);
    assert!(failed.payload["reason"]
        .as_str()
        .unwrap()
        .contains("orq-no-such-harness"));
}

#[test]
fn the_session_sees_only_the_required_environment_plus_what_it_is_given() {
    let s = setup("env");
    // Cargo sets this for the test process; it must not reach the session.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let args = argv(&[
        "-c",
        r#"echo "manifest=[$CARGO_MANIFEST_DIR]"; echo "given=[$ORQ_GIVEN]"; echo "path=[${PATH:+set}]""#,
    ]);
    let given = [("ORQ_GIVEN".to_string(), "yes".to_string())];

    run_implement_stage(&s.store, "run-1", &session(&s, &args, &given)).unwrap();

    let log = fs::read_to_string(&s.log).unwrap();
    assert!(log.contains("manifest=[]\n"), "{log}");
    assert!(log.contains("given=[yes]\n"), "{log}");
    assert!(log.contains("path=[set]\n"), "{log}");
}

#[test]
fn an_unknown_run_starts_nothing() {
    let s = setup("unknown");
    let marker = s.cwd.join("ran");
    let args = argv(&["-c", "touch ran"]);

    let result = run_implement_stage(&s.store, "ghost", &session(&s, &args, &[]));

    assert!(matches!(result, Err(StoreError::NotFound(_))));
    assert!(!Path::new(&marker).exists());
}

#[test]
fn a_terminal_session_runs_attached_and_still_records_its_outcome() {
    let s = setup("terminal");
    let args = argv(&["-c", "exit 0"]);
    let mut request = session(&s, &args, &[]);
    request.terminal = true;

    let outcome = run_implement_stage(&s.store, "run-1", &request).unwrap();

    assert!(outcome.success);
    assert_eq!(kinds(&s.store), ["stage.started", "stage.completed"]);
}

#[cfg(unix)]
#[test]
fn a_session_killed_by_sigkill_names_the_signal_and_points_at_the_out_of_memory_killer() {
    let s = setup("sigkill");
    let args = argv(&["-c", "kill -KILL $$"]);

    let outcome = run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, None);
    let failed = &s.store.events("run-1").unwrap()[1];
    assert_eq!(failed.kind, "stage.failed");
    assert_eq!(failed.payload["exit_code"], serde_json::Value::Null);
    assert_eq!(failed.payload["signal"], 9);
    assert_eq!(
        failed.payload["reason"],
        "sh was stopped by signal 9 (SIGKILL) — often the kernel's out-of-memory killer; check dmesg"
    );
}

#[test]
fn a_terminal_session_leaves_a_log_saying_when_it_started_and_how_it_ended() {
    let s = setup("terminal-log");
    let args = argv(&["-c", "exit 0"]);
    let mut request = session(&s, &args, &[]);
    request.terminal = true;

    run_implement_stage(&s.store, "run-1", &request).unwrap();

    let log = fs::read_to_string(&s.log).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].starts_with("[orq "), "{log}");
    assert!(
        lines[0].contains("started sh (pid ")
            && lines[0].ends_with(&format!("in {}", s.cwd.display())),
        "{log}"
    );
    assert!(
        lines[1].starts_with("[orq ") && lines[1].ends_with("sh exited with status 0"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn a_terminal_session_killed_by_a_signal_logs_the_same_reason_as_its_event() {
    let s = setup("terminal-signal");
    let args = argv(&["-c", "kill -KILL $$"]);
    let mut request = session(&s, &args, &[]);
    request.terminal = true;

    run_implement_stage(&s.store, "run-1", &request).unwrap();

    let reason = s.store.events("run-1").unwrap()[1].payload["reason"]
        .as_str()
        .unwrap()
        .to_string();
    let log = fs::read_to_string(&s.log).unwrap();
    let last = log.lines().last().unwrap();
    assert!(
        last.starts_with("[orq ") && last.ends_with(&format!("] {reason}")),
        "{log}"
    );
}

#[test]
fn a_terminal_session_closed_through_its_exit_marker_logs_that_orq_closed_it() {
    let s = setup("terminal-closed");
    let marker = s.cwd.join("done");
    let script = format!("touch {}; sleep 30", marker.display());
    let args = argv(&["-c", &script]);
    let mut request = session(&s, &args, &[]);
    request.terminal = true;
    request.exit_marker = Some(&marker);

    let outcome = run_implement_stage(&s.store, "run-1", &request).unwrap();

    assert!(outcome.success);
    let log = fs::read_to_string(&s.log).unwrap();
    let last = log.lines().last().unwrap();
    assert!(
        last.ends_with("sh was closed by orq after it wrote its exit marker"),
        "{log}"
    );
}

#[test]
fn a_terminal_session_that_cannot_start_logs_why_and_no_start_line() {
    let s = setup("terminal-no-start");
    let args = argv(&[]);
    let mut request = session(&s, &args, &[]);
    request.program = "/nonexistent/orq-no-such-program";
    request.terminal = true;

    let outcome = run_implement_stage(&s.store, "run-1", &request).unwrap();

    assert!(!outcome.success);
    let failed = &s.store.events("run-1").unwrap()[1];
    assert_eq!(failed.kind, "stage.failed");
    let reason = failed.payload["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("could not start /nonexistent/orq-no-such-program: "),
        "{reason}"
    );
    let log = fs::read_to_string(&s.log).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(
        lines[0].starts_with("[orq ") && lines[0].ends_with(&format!("] {reason}")),
        "{log}"
    );
}

#[test]
fn a_terminal_session_whose_marker_reports_a_problem_logs_how_it_ended_and_the_problem() {
    let s = setup("terminal-problem");
    let marker = s.cwd.join("done");
    let script = format!(
        "printf 'PROBLEM\\nthe tests fail\\n' > {}; sleep 30",
        marker.display()
    );
    let args = argv(&["-c", &script]);
    let mut request = session(&s, &args, &[]);
    request.terminal = true;
    request.exit_marker = Some(&marker);

    let outcome = run_implement_stage(&s.store, "run-1", &request).unwrap();

    assert!(!outcome.success);
    let reason = s.store.events("run-1").unwrap()[1].payload["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(reason, "the session reported a problem: the tests fail");
    let log = fs::read_to_string(&s.log).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "{log}");
    assert!(lines[0].contains("started sh (pid "), "{log}");
    assert!(
        lines[1].ends_with("] sh was closed by orq after it wrote its exit marker"),
        "{log}"
    );
    assert!(lines[2].ends_with(&format!("] {reason}")), "{log}");
}

#[test]
fn a_session_start_records_how_many_lines_its_log_already_has() {
    let s = setup("log-line");
    let args = argv(&["-c", "echo one; echo two"]);

    run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();
    // A later attempt appends to the same log, after the first attempt's two lines.
    run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    let starts: Vec<_> = s
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "stage.started")
        .map(|e| e.payload["log_line"].clone())
        .collect();
    assert_eq!(starts, [serde_json::json!(0), serde_json::json!(2)]);
    let offsets: Vec<_> = s
        .store
        .events("run-1")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "stage.started")
        .map(|e| e.payload["log_offset"].clone())
        .collect();
    assert_eq!(
        offsets,
        [serde_json::json!(0), serde_json::json!("one\ntwo\n".len())]
    );
}
