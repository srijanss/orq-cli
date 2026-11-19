//! Each Accept writes what its test steps printed, whole, to `accept-<n>.log` in the run's log directory, and its
//! failure names that log, the tests that failed with their own output, and a headline that says what failed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{accept_run_by, run_steps, Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A failed run with one commit beyond its base, and the root of its store.
fn failed_run(tag: &str) -> (GitFixture, PathBuf, RunStore) {
    let repo = GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    std::fs::write(worktree.join("work.txt"), "work").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "work"]);
    let root = orq_test_support::process_temp_dir().join(format!("orq-accept-log-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: base,
        branch: "orq-run-1".into(),
        worktree,
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.start_run(&record).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e-1".into(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "run.completed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-19T10:00:00Z".into(),
            payload: serde_json::json!({ "ending": "failed", "reason": "the gate failed" }),
        })
        .unwrap();
    (repo, root, store)
}

const FAILING: &str = "orq-tui::ui_status unknown_statuses_are_listed";

/// A nextest run whose one failure, with its panic, is printed long before its last 400 passing lines.
fn nextest_script() -> String {
    format!(
        "echo 'first line of the run'; \
         echo '        FAIL [   0.010s] (1/401) {FAILING}'; \
         echo '──── STDERR:             {FAILING}'; \
         echo \"thread 'unknown_statuses_are_listed' panicked at tests/ui_status.rs:12:5:\"; \
         echo 'assertion `left == right` failed'; \
         echo '  left: 1'; \
         echo ' right: 2'; \
         i=2; while [ $i -le 401 ]; do echo \"        PASS [   0.004s] ($i/401) orq-core::suite test_$i\"; i=$((i+1)); done; \
         echo '     Summary [   1.000s] 401 tests run: 400 passed, 1 failed'; \
         echo '        FAIL [   0.010s] (1/401) {FAILING}'; \
         echo '        PASS [   0.004s] (401/401) orq-core::suite last'; \
         exit 100"
    )
}

fn failing_tests(worktree: &Path) -> Result<(), String> {
    run_steps(worktree, &[vec!["sh".into(), "-c".into(), nextest_script()]])
}

fn last_event(store: &RunStore) -> Event {
    store.events("run-1").unwrap().pop().unwrap()
}

#[test]
fn a_failing_accept_writes_its_whole_output_to_accept_1_log_and_the_event_names_it_and_its_failed_tests() {
    let (_repo, root, store) = failed_run("first");

    accept_run_by(&store, "run-1", "web", &failing_tests).unwrap_err();

    let log = root.join("logs").join("run-1").join("accept-1.log");
    let text = std::fs::read_to_string(&log).expect("accept-1.log");
    assert!(text.contains("first line of the run"), "the whole output, its start included");
    assert!(text.contains("panicked at tests/ui_status.rs:12:5"), "{text}");
    assert!(text.contains("(401/401) orq-core::suite last"), "{text}");
    let failed = last_event(&store);
    assert_eq!(failed.kind, "run.accept_failed");
    assert_eq!(failed.payload["log"].as_str().map(Path::new), Some(log.as_path()));
    assert_eq!(failed.payload["failed_tests"][0]["name"], FAILING);
    let output = failed.payload["failed_tests"][0]["output"].as_str().unwrap();
    assert!(output.contains("left: 1") && output.contains("right: 2"), "{output}");
    assert_eq!(failed.payload["failed_tests_more"], 0);
    assert_eq!(failed.payload["reason"], format!("1 test failed: {FAILING}"));
}

#[test]
fn a_second_accept_writes_accept_2_log_and_a_passing_one_names_its_log_too() {
    let (_repo, root, store) = failed_run("second");
    accept_run_by(&store, "run-1", "web", &failing_tests).unwrap_err();

    accept_run_by(&store, "run-1", "web", &|dir: &Path| run_steps(dir, &[vec!["sh".into(), "-c".into(), "echo all good".into()]])).unwrap();

    let log = root.join("logs").join("run-1").join("accept-2.log");
    assert!(std::fs::read_to_string(&log).unwrap().contains("all good"));
    let completed = last_event(&store);
    assert_eq!(completed.kind, "run.completed");
    assert_eq!(completed.payload["log"].as_str().map(Path::new), Some(log.as_path()));
}

#[test]
fn the_reason_is_never_a_pass_line_and_without_test_names_says_how_the_step_ended() {
    let (_repo, _root, store) = failed_run("no-names");
    // The step's own command must not carry the word, only what it prints.
    let script = "printf '        P%sS [   0.004s] (1/2) orq-core::suite one\\n' AS; echo 'error: could not compile `orq-core`'; exit 101";

    accept_run_by(&store, "run-1", "web", &|dir: &Path| run_steps(dir, &[vec!["sh".into(), "-c".into(), script.into()]])).unwrap_err();

    let reason = last_event(&store).payload["reason"].as_str().unwrap().to_string();
    assert!(!reason.contains("PASS"), "{reason}");
    assert!(reason.ends_with("exited with status 101"), "{reason}");
}

#[test]
fn a_test_that_failed_a_try_but_passed_on_retry_is_not_listed() {
    let (_repo, _root, store) = failed_run("retry");
    let script = "echo '   TRY 1 FAIL [   0.010s] (1/2) suite flaky'; \
                  echo '   TRY 2 PASS [   0.010s] (1/2) suite flaky'; \
                  echo '        FAIL [   0.010s] (2/2) suite broken'; exit 100";

    accept_run_by(&store, "run-1", "web", &|dir: &Path| run_steps(dir, &[vec!["sh".into(), "-c".into(), script.into()]])).unwrap_err();

    let failed = last_event(&store);
    let names: Vec<&str> = failed.payload["failed_tests"].as_array().unwrap().iter().map(|test| test["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["suite broken"]);
}
