//! When a web Start or Resume fails for a reason the browser is not told, the server's own log gets it:
//! one line naming the action, project and feature or run, with the reason escaped and bounded.

use orq_core::remote_inbox::{handle_with, launch_failure_line, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

/// Set in a child run of this test binary that does the handler work, so the parent can see its stderr.
const CHILD: &str = "ORQ_LAUNCH_FAILURE_LOG_CHILD";

/// A provider whose start and resume fail with a reason the browser must not see.
struct Broken;

impl ProjectsProvider for Broken {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, _feature: &str) -> Result<RunStart, String> {
        Err("/home/me/secret/start exploded".into())
    }

    fn control_run(&self, _id: &str, _run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        Err("/home/me/secret/resume exploded".into())
    }
}

/// The handler work, done only inside a child: a failed start and a failed resume, each still answered
/// with the generic 500.
#[test]
fn child_a_failed_start_and_resume() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let base = orq_test_support::process_temp_dir().join(format!("orq-launchlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let config = RemoteConfig { token: "tok".into() };
    let post = |path: &str| {
        let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let reply = handle_with(&roots, &config, "p", "p", &request, &Broken);
        (reply.status, reply.body)
    };
    assert_eq!(post("/api/projects/p-1/features/ready/runs"), (500, json!({"error": "the run could not be started"})));
    assert_eq!(post("/api/projects/p-1/runs/r-1/resume"), (500, json!({"error": "the run could not be resumed"})));

    // Both attempts are audited, whatever became of the log, with the generic reason and never the private one.
    let audited: Vec<(String, String, String)> = orq_core::remote_audit::read(&roots, 10)
        .unwrap()
        .iter()
        .rev()
        .map(|line| [&line["action"], &line["outcome"], &line["reason"]].map(|field| field.as_str().unwrap_or("").to_string()).into())
        .collect();
    assert_eq!(
        audited,
        [
            ("run.start".to_string(), "refused".to_string(), "the run could not be started".to_string()),
            ("run.resume".to_string(), "refused".to_string(), "the run could not be resumed".to_string()),
        ]
    );
}

/// Runs `child_a_failed_start_and_resume` in a child of this test binary with its stderr sent to `stderr`.
fn run_child(stderr: std::process::Stdio) -> std::process::Output {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "child_a_failed_start_and_resume", "--nocapture", "--test-threads", "1"]);
    child.env(CHILD, "1");
    child.stdout(std::process::Stdio::piped()).stderr(stderr).output().unwrap()
}

/// The log is the operator's diagnostic, never a reason to drop the request: a log that cannot be written
/// still leaves the start and the resume answered. `/dev/full`, whose writes always fail, is Linux's.
#[cfg(target_os = "linux")]
#[test]
fn a_log_that_cannot_be_written_does_not_stop_the_failure_being_answered() {
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let out = run_child(full.into());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn a_failed_launch_is_one_log_line_naming_the_action_target_and_the_real_reason() {
    assert_eq!(
        launch_failure_line("run.start", "orq-cli", "ready", "cannot start orq run: No such file or directory (os error 2)"),
        "orq-remote: run.start orq-cli ready failed: cannot start orq run: No such file or directory (os error 2)"
    );
}

#[test]
fn a_reason_with_line_breaks_or_control_characters_stays_on_one_escaped_line() {
    let line = launch_failure_line("run.resume", "p", "run-1", "first\nforged: line\r\u{1b}[31mred");
    assert_eq!(line, "orq-remote: run.resume p run-1 failed: first\\nforged: line\\r\\u{1b}[31mred");
    assert_eq!(line.lines().count(), 1);
}

#[test]
fn a_very_long_reason_is_cut_short() {
    let line = launch_failure_line("run.start", "p", "f", &"x".repeat(10_000));
    assert!(line.len() < 1_200, "{}", line.len());
    assert!(line.ends_with('…'), "{line}");
}

/// The real reason reaches the server's log from the handler itself, for a start and for a resume, while
/// the browser is answered with the generic 500 (checked in the child).
#[test]
fn a_failed_start_and_resume_put_the_real_reason_in_the_server_log() {
    let out = run_child(std::process::Stdio::piped());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("orq-remote: run.start p-1 ready failed: /home/me/secret/start exploded\n"), "{log}");
    assert!(log.contains("orq-remote: run.resume p-1 r-1 failed: /home/me/secret/resume exploded\n"), "{log}");
}
