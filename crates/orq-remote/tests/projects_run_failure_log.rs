//! A failure event on a run's timeline names the log it wrote and the line in that log where this attempt's
//! error is, so the page can open the log there instead of at the top of the first file.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    project: String,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let project = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    Setup { scan, roots, project }
}

fn logs_dir(s: &Setup, run: &str) -> PathBuf {
    s.roots.state.join("logs").join(run)
}

fn write_log(s: &Setup, run: &str, name: &str, text: &str) -> PathBuf {
    let dir = logs_dir(s, run);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// A run with the given events, each `(kind, stage, payload)`.
fn add_run(s: &Setup, run: &str, events: &[(&str, Option<&str>, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: s.project.clone(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, stage, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: s.project.clone(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: stage.map(String::from), attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload: payload.clone() };
        store.append_event(&event).unwrap();
    }
}

fn timeline(s: &Setup, run: &str) -> Vec<Value> {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}");
    let request = Request { method: "GET", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body["events"].as_array().unwrap().clone()
}

fn failures(events: &[Value]) -> Vec<Value> {
    events.iter().filter(|e| e["kind"].as_str().unwrap().ends_with(".failed")).map(|e| e["failure"].clone()).collect()
}

/// One gate attempt's output: its first line, `filler` lines of passing output with noise in them, then
/// its own failure lines.
fn attempt(filler: usize, failure: &[&str]) -> Vec<String> {
    let mut lines = vec!["$ cargo test --workspace".to_string()];
    for n in 0..filler {
        lines.push(if n % 7 == 3 { format!("orq-remote: run.accept {n} failed: the branch moved") } else { format!("test passing_{n} ... ok") });
    }
    lines.extend(failure.iter().map(|line| line.to_string()));
    lines.push("error: test failed, to rerun pass `-p orq-remote --test x`".to_string());
    lines
}

fn text(lines: &[String]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn each_failed_gate_attempt_names_the_first_error_line_of_its_own_part_of_the_appended_log() {
    let s = setup("failure-attempts");
    let one = attempt(30, &["test alpha::one ... FAILED", "", "thread 'alpha::one' panicked at src/a.rs:1:1:"]);
    let two = attempt(50, &["thread 'beta::two' panicked at src/b.rs:2:2:"]);
    let three = attempt(20, &["---- gamma::three stdout ----", "thread 'gamma::three' panicked at src/c.rs:3:3:"]);
    let mut all = one.clone();
    all.extend(two.clone());
    all.extend(three.clone());
    let log = path_str(&write_log(&s, "r1", "gate.log", &text(&all)));
    let reason = "gate step 1 (cargo) exited with status 101";
    let started = |at: usize| json!({"log": log, "steps": 1, "log_line": at});
    let failed = json!({"log": log, "exit_code": 101, "reason": reason});
    add_run(&s, "r1", &[
        ("stage.started", Some("verify"), started(0)),
        ("stage.failed", Some("verify"), failed.clone()),
        ("stage.started", Some("verify"), started(one.len())),
        ("stage.failed", Some("verify"), failed.clone()),
        ("stage.started", Some("verify"), started(one.len() + two.len())),
        ("stage.failed", Some("verify"), failed.clone()),
    ]);
    let found = failures(&timeline(&s, "r1"));
    // Lines are numbered from 1, as an editor numbers them.
    let first = one.iter().position(|l| l == "test alpha::one ... FAILED").unwrap() + 1;
    let second = one.len() + two.iter().position(|l| l.contains("panicked at")).unwrap() + 1;
    let third = one.len() + two.len() + three.iter().position(|l| l.starts_with("---- gamma")).unwrap() + 1;
    let lines: Vec<_> = found.iter().map(|f| f["log"]["line"].clone()).collect();
    assert_eq!(lines, [json!(first), json!(second), json!(third)], "{found:?}");
    for f in &found {
        assert_eq!(f["reason"], reason, "{f}");
        assert_eq!(f["log"]["file"], "gate.log", "{f}");
    }
    let excerpt = found[0]["log"]["excerpt"].as_str().unwrap();
    assert!(excerpt.starts_with("test alpha::one ... FAILED\n\nthread 'alpha::one' panicked at"), "{excerpt}");
    assert_eq!(excerpt.lines().count(), 4, "the excerpt runs on to the end of the part at most: {excerpt}");
}

#[test]
fn without_a_recorded_line_count_an_attempt_is_found_by_the_logs_repeated_first_line() {
    let s = setup("failure-legacy");
    let one = attempt(10, &["thread 'alpha::one' panicked at src/a.rs:1:1:"]);
    let two = attempt(25, &["thread 'beta::two' panicked at src/b.rs:2:2:"]);
    let mut all = one.clone();
    all.extend(two.clone());
    let log = path_str(&write_log(&s, "r1", "gate.log", &text(&all)));
    let started = json!({"log": log, "steps": 1});
    let failed = json!({"log": log, "exit_code": 101, "reason": "gate step 1 (cargo) exited with status 101"});
    add_run(&s, "r1", &[
        ("stage.started", Some("verify"), started.clone()),
        ("stage.failed", Some("verify"), failed.clone()),
        ("stage.started", Some("verify"), started.clone()),
        ("stage.failed", Some("verify"), failed.clone()),
    ]);
    let found = failures(&timeline(&s, "r1"));
    let first = one.iter().position(|l| l.contains("panicked at")).unwrap() + 1;
    let second = one.len() + two.iter().position(|l| l.contains("panicked at")).unwrap() + 1;
    let lines: Vec<_> = found.iter().map(|f| f["log"]["line"].clone()).collect();
    assert_eq!(lines, [json!(first), json!(second)], "{found:?}");
}

#[test]
fn when_the_attempts_cannot_be_told_apart_the_last_error_in_the_whole_log_is_used() {
    let s = setup("failure-unsplit");
    let mut all = vec!["orq: nextest is not installed".to_string()];
    all.extend(attempt(5, &["thread 'alpha::one' panicked at src/a.rs:1:1:"]));
    all.extend(attempt(5, &["thread 'beta::two' panicked at src/b.rs:2:2:"]));
    let log = path_str(&write_log(&s, "r1", "gate.log", &text(&all)));
    // Two starts, but the log's first line appears once: its attempts cannot be counted.
    add_run(&s, "r1", &[
        ("stage.started", Some("verify"), json!({"log": log})),
        ("stage.started", Some("verify"), json!({"log": log})),
        ("stage.failed", Some("verify"), json!({"log": log, "reason": "x"})),
    ]);
    let found = failures(&timeline(&s, "r1"));
    let last = all.iter().rposition(|l| l.starts_with("error: test failed")).unwrap() + 1;
    assert_eq!(found[0]["log"]["line"], json!(last), "{found:?}");
}

/// The line `find_failure_line` gives for a single-attempt log of `lines`.
fn line_in(label: &str, lines: &[&str]) -> Option<usize> {
    let s = setup(label);
    let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let path = write_log(&s, "r1", "test.log", &text(&lines));
    orq_remote::projects::find_failure_line(&path, orq_remote::projects::LogPart::Lines { start: 0, end: None }).unwrap().map(|spot| spot.line)
}

#[test]
fn python_and_javascript_test_failures_are_found() {
    let pytest = ["$ pytest", "collected 3 items", "tests/test_a.py ..F", "Traceback (most recent call last):", "  File \"a.py\", line 1", "ValueError: bad", "FAILED tests/test_a.py::test_c - ValueError: bad", "===== 1 failed, 2 passed in 0.1s ====="];
    assert_eq!(line_in("failure-traceback", &pytest), Some(6), "an error line after a traceback");
    assert_eq!(line_in("failure-pytest", &["$ pytest", "tests/test_a.py ..F", "FAILED tests/test_a.py::test_c - assert 1 == 2"]), Some(3));
    assert_eq!(line_in("failure-pytest-summary", &["$ pytest -q", "..", "= 1 failed, 2 passed in 0.1s ="]), Some(3));
    assert_eq!(line_in("failure-vitest", &["$ npx vitest run", " ✓ src/a.test.js (2)", " FAIL  src/b.test.js > adds", "AssertionError: expected 1 to be 2"]), Some(3));
    assert_eq!(line_in("failure-jest-cross", &["$ npx jest", "  ✕ adds (3 ms)"]), Some(2));
    assert_eq!(line_in("failure-times", &["$ npx vitest", "   × adds 2×2"]), Some(2));
    assert_eq!(line_in("failure-assertion", &["$ node test.js", "node:assert AssertionError [ERR_ASSERTION]: 1 == 2"]), Some(2));
    assert_eq!(line_in("failure-shell", &["$ make check", "gate step 2 (make) exited with status 2"]), Some(2));
    let noise = ["$ cargo test", "orq-remote: run.accept 3 failed: moved", "an error was logged", "a Error here", "2×2 grid", "FAILED. it said", "thread 'a' panicked at src/a.rs:1:1:"];
    assert_eq!(line_in("failure-noise", &noise), Some(7), "noise is no failure line");
    assert_eq!(line_in("failure-weak", &["$ cargo test", "gate step 1 (sh) exited with status 1", "test a ... FAILED"]), Some(3), "a tool's own failure line comes before a command's exit status");
}

#[test]
fn a_log_with_no_failure_line_points_forty_lines_before_the_end_of_its_part() {
    let s = setup("failure-none");
    let lines: Vec<String> = (1..=100).map(|n| format!("step {n} ok")).collect();
    let log = path_str(&write_log(&s, "r1", "implement.log", &text(&lines)));
    add_run(&s, "r1", &[
        ("stage.started", Some("implement"), json!({"log": log, "log_line": 0})),
        ("stage.failed", Some("implement"), json!({"log": log, "reason": "the session exited with status 1"})),
    ]);
    let found = failures(&timeline(&s, "r1"));
    assert_eq!(found[0]["log"]["line"], json!(60), "{found:?}");
    assert!(found[0]["log"]["excerpt"].as_str().unwrap().starts_with("step 60 ok\nstep 61 ok"), "{found:?}");
    assert_eq!(found[0]["log"]["excerpt"].as_str().unwrap().lines().count(), 15, "{found:?}");

    let s = setup("failure-short");
    let log = path_str(&write_log(&s, "r1", "implement.log", "one\ntwo\n"));
    add_run(&s, "r1", &[("stage.started", Some("implement"), json!({"log": log, "log_line": 0})), ("run.failed", None, json!({"log": log, "reason": "x"}))]);
    assert_eq!(failures(&timeline(&s, "r1"))[0]["log"]["line"], json!(1), "a short part points at its first line");
}

#[test]
fn a_log_outside_the_runs_own_logs_or_a_link_or_an_odd_name_gives_no_log() {
    let s = setup("failure-outside");
    let body = "$ cargo test\nthread 'a' panicked at src/a.rs:1:1:\n";
    let other = path_str(&write_log(&s, "r2", "gate.log", body));
    let real = write_log(&s, "r1", "real.log", body);
    let link = logs_dir(&s, "r1").join("link.log");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let hidden = path_str(&write_log(&s, "r1", ".hidden.log", body));
    let text_file = path_str(&write_log(&s, "r1", "notes.txt", body));
    let outside = s.roots.state.join("elsewhere.log");
    std::fs::write(&outside, body).unwrap();
    let dotted = format!("{}/../r2/gate.log", path_str(&logs_dir(&s, "r1")));
    let mut events = Vec::new();
    for log in [other, path_str(&link), hidden, text_file, path_str(&outside), dotted, "gate.log".to_string()] {
        events.push(("stage.failed", Some("verify"), json!({"log": log, "reason": "no"})));
    }
    events.push(("stage.failed", Some("verify"), json!({"log": path_str(&real), "reason": "yes"})));
    add_run(&s, "r1", &events);
    let found = failures(&timeline(&s, "r1"));
    for f in &found[..7] {
        assert_eq!(f["reason"], "no", "{f}");
        assert!(f.get("log").is_none(), "{f}");
    }
    assert_eq!(found[7]["log"]["file"], "real.log", "{:?}", found[7]);
    assert_eq!(found[7]["log"]["line"], json!(2), "{:?}", found[7]);
}

#[test]
fn the_reason_and_excerpt_are_redacted() {
    let s = setup("failure-redacted");
    let log = path_str(&write_log(&s, "r1", "gate.log", "$ cargo test\nthread 'a' panicked at src/a.rs:1:1:\nGITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwx\n"));
    add_run(&s, "r1", &[("stage.failed", Some("verify"), json!({"log": log, "reason": "failed with Authorization: Bearer abcdefghijklmnop12345"}))]);
    let found = failures(&timeline(&s, "r1"));
    let shown = found[0].to_string();
    assert!(!shown.contains("abcdefghijklmnop12345") && !shown.contains("ghp_"), "{shown}");
    assert!(found[0]["log"]["excerpt"].as_str().unwrap().contains("panicked at"), "{shown}");
}

#[test]
fn a_log_over_twenty_megabytes_is_named_but_not_searched() {
    let s = setup("failure-huge");
    let path = write_log(&s, "r1", "gate.log", "$ cargo test\nthread 'a' panicked at src/a.rs:1:1:\n");
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().set_len(20 * 1024 * 1024 + 1).unwrap();
    add_run(&s, "r1", &[("stage.failed", Some("verify"), json!({"log": path_str(&path), "reason": "big"}))]);
    let found = failures(&timeline(&s, "r1"));
    assert_eq!(found[0]["log"], json!({"file": "gate.log"}), "{found:?}");
}

#[test]
fn a_failure_line_past_what_the_logs_page_can_show_is_not_given() {
    let s = setup("failure-past-cap");
    let filler = "x".repeat(1023) + "\n";
    let mut body = "$ cargo test\n".to_string();
    body.push_str(&filler.repeat(9 * 1024));
    body.push_str("thread 'a' panicked at src/a.rs:1:1:\n");
    let path = write_log(&s, "r1", "gate.log", &body);
    add_run(&s, "r1", &[("stage.failed", Some("verify"), json!({"log": path_str(&path), "reason": "late"}))]);
    let found = failures(&timeline(&s, "r1"));
    assert!(found[0]["log"].get("line").is_none(), "the Logs tab reads only the first 8 MB: {found:?}");
    assert!(found[0]["log"]["excerpt"].as_str().unwrap().contains("panicked at"), "{found:?}");
}

#[test]
fn a_failure_without_a_log_still_gives_its_redacted_reason() {
    let s = setup("failure-no-log");
    add_run(&s, "r1", &[
        ("stage.failed", Some("verify"), json!({"reason": "could not start: token=ghp_abcdefghijklmnopqrstuvwxyz0123456789"})),
        ("stage.failed", Some("verify"), json!({"reason": "bad log", "log": 7})),
        ("stage.failed", Some("verify"), json!({"exit_code": 1})),
    ]);
    let found = failures(&timeline(&s, "r1"));
    assert!(found[0]["reason"].as_str().unwrap().starts_with("could not start"), "{:?}", found[0]);
    assert!(!found[0]["reason"].as_str().unwrap().contains("ghp_abcdef"), "{:?}", found[0]);
    assert!(found[0].get("log").is_none(), "{:?}", found[0]);
    assert_eq!(found[1], json!({"reason": "bad log"}));
    // Nothing to say gives no failure at all.
    assert!(found[2].is_null(), "{:?}", found[2]);
}

#[test]
fn an_attempt_with_a_recorded_byte_offset_is_read_from_there_without_rereading_the_lines_before_it() {
    let s = setup("failure-offset");
    let before = "$ cargo test\nthread 'early' panicked at src/a.rs:1:1:\nend of earlier attempt\n";
    let after = "$ cargo test\nthread 'later' panicked at src/b.rs:2:2:\n";
    let log = path_str(&write_log(&s, "r1", "gate.log", &format!("{before}{after}")));
    // The start says one line came before, though three did: only the byte offset is used to find the
    // attempt, and its lines are numbered on from the recorded count.
    add_run(&s, "r1", &[
        ("stage.started", Some("verify"), json!({"log": log, "log_line": 1, "log_offset": before.len()})),
        ("stage.failed", Some("verify"), json!({"log": log, "reason": "gate failed"})),
    ]);
    let found = failures(&timeline(&s, "r1"));
    assert_eq!(found[0]["log"]["line"], json!(3), "{:?}", found[0]);
    assert!(found[0]["log"]["excerpt"].as_str().unwrap().starts_with("thread 'later' panicked"), "{:?}", found[0]);
}

#[test]
fn an_empty_log_is_named_without_a_line_and_an_unreadable_one_gives_only_the_reason() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("failure-error-paths");
    let empty = path_str(&write_log(&s, "r1", "empty.log", ""));
    let locked_path = write_log(&s, "r1", "locked.log", "thread 'x' panicked at src/a.rs:1:1:\n");
    std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let locked = path_str(&locked_path);
    add_run(&s, "r1", &[
        ("stage.started", Some("verify"), json!({"log": empty, "log_line": 0, "log_offset": 0})),
        ("stage.failed", Some("verify"), json!({"log": empty, "reason": "empty"})),
        ("stage.started", Some("verify"), json!({"log": locked, "log_line": 0, "log_offset": 0})),
        ("stage.failed", Some("verify"), json!({"log": locked, "reason": "locked"})),
    ]);
    let found = failures(&timeline(&s, "r1"));
    std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(found[0], json!({"reason": "empty", "log": {"file": "empty.log"}}));
    // Root reads a file whatever its mode, so only the empty log is checked there.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(found[1], json!({"reason": "locked"}));
    }
}
