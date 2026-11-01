use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{run_implement_stage, ImplementSession, RoleProfile, RunRecord, RunStore, StoreError};

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-implement-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(run_id: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile { harness: "claude".to_string(), model: "opus".to_string(), thinking: None },
    );
    RunRecord {
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "git@github.com:acme/app.git".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--COM-02".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--COM-02"),
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
    Setup { store, cwd, log: root.join("implement.log") }
}

fn session<'a>(s: &'a Setup, args: &'a [String], env: &'a [(String, String)]) -> ImplementSession<'a> {
    ImplementSession {
        program: "sh",
        args,
        env,
        cwd: &s.cwd,
        session_id: Some("sess-42"),
        log: &s.log,
        terminal: false,
    }
}

fn kinds(store: &RunStore) -> Vec<String> {
    store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn a_finished_session_records_start_and_completion_with_its_session_log_and_exit_code() {
    let s = setup("ok");
    let args = argv(&["-c", r#"pwd; echo out-""ok; echo err-""ok >&2"#]);

    let outcome = run_implement_stage(&s.store, "run-1", &session(&s, &args, &[])).unwrap();

    assert!(outcome.success);
    assert_eq!(outcome.exit_code, Some(0));
    let events = s.store.events("run-1").unwrap();
    assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["stage.started", "stage.completed"]);
    for event in &events {
        assert_eq!(event.stage.as_deref(), Some("implement_and_review"));
        assert_eq!(event.attempt, 1);
        assert_eq!((event.run_id.as_str(), event.project_id.as_str(), event.feature_id.as_str()), ("run-1", "repo-1", "COM-02"));
        assert_eq!(event.payload["session_id"], "sess-42");
        assert_eq!(event.payload["log"], s.log.to_str().unwrap());
    }
    assert_ne!(events[0].event_id, events[1].event_id);
    assert_eq!(events[1].payload["exit_code"], 0);

    // Run in the worktree, with both streams captured in the log.
    let log = fs::read_to_string(&s.log).unwrap();
    assert!(log.contains(fs::canonicalize(&s.cwd).unwrap().to_str().unwrap()), "{log}");
    assert!(log.contains("out-ok\n") && log.contains("err-ok\n"), "{log}");
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
    assert!(failed.payload["reason"].as_str().unwrap().contains("orq-no-such-harness"));
}

#[test]
fn the_session_sees_only_the_required_environment_plus_what_it_is_given() {
    let s = setup("env");
    // Cargo sets this for the test process; it must not reach the session.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let args = argv(&["-c", r#"echo "manifest=[$CARGO_MANIFEST_DIR]"; echo "given=[$ORQ_GIVEN]"; echo "path=[${PATH:+set}]""#]);
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
