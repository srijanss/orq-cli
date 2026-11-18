//! Accepting a run is recorded: when it started and on which head, then whether its tests passed or why they
//! failed, so every device sees the run accepting and what came of it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{accept_run_by, fail_interrupted_accept, summarize_run, Event, RunPhase, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A run with one commit beyond its base, ended by `ending` (`failed` or `needs_review`).
fn ended_run(tag: &str, ending: &str) -> (GitFixture, PathBuf, RunStore) {
    let repo = GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    std::fs::write(worktree.join("work.txt"), "work").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "work"]);
    let root = orq_test_support::process_temp_dir().join(format!("orq-accept-events-{}-{tag}", std::process::id()));
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
        worktree: worktree.clone(),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.start_run(&record).unwrap();
    store.append_event(&event("run.completed", serde_json::json!({ "ending": ending, "reason": "the review escalated" }))).unwrap();
    (repo, worktree, store)
}

fn kinds(store: &RunStore) -> Vec<String> {
    store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn a_passing_accept_records_its_start_then_ends_the_run_review_ready() {
    let (_repo, worktree, store) = ended_run("pass", "needs_review");
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    let seen = RefCell::new(None);

    accept_run_by(&store, "run-1", "web", &|_: &Path| {
        // While the tests run, the run reads as accepting.
        *seen.borrow_mut() = Some(summarize_run(&store.events("run-1").unwrap()).phase);
        Ok(())
    })
    .unwrap();

    assert_eq!(seen.into_inner(), Some(RunPhase::Accepting));
    let events = store.events("run-1").unwrap();
    let started = events.iter().find(|e| e.kind == "run.accept_started").expect("run.accept_started");
    assert_eq!(started.payload["by"], "web");
    assert_eq!(started.payload["head"], head.as_str());
    let last = events.last().unwrap();
    assert_eq!(last.kind, "run.completed");
    assert_eq!(last.payload["ending"], "review_ready");
    assert_eq!(last.payload["note"], "accepted: tests passed");
    let summary = summarize_run(&events);
    assert_eq!(summary.phase, RunPhase::Completed);
    assert_eq!(summary.accept_failure, None);
}

#[test]
fn a_failing_accept_records_why_with_the_output_tail_and_the_run_returns_to_what_it_was() {
    for ending in ["needs_review", "failed"] {
        let (_repo, _worktree, store) = ended_run(&format!("fail-{ending}"), ending);
        let output = format!("{}\ntest result: FAILED. 2 failed\nTOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789", "noise line\n".repeat(400));

        let err = accept_run_by(&store, "run-1", "web", &|_: &Path| Err(format!("`cargo test` failed: {output}"))).unwrap_err();

        assert!(err.contains("cargo test"), "{err}");
        assert_eq!(kinds(&store)[kinds(&store).len() - 2..], ["run.accept_started", "run.accept_failed"]);
        let summary = summarize_run(&store.events("run-1").unwrap());
        let expected = if ending == "failed" { RunPhase::Failed } else { RunPhase::NeedsReview };
        assert_eq!(summary.phase, expected);
        assert_eq!(summary.reason.as_deref(), Some("the review escalated"), "the run's own reason stays");
        let failure = summary.accept_failure.expect("the accept failure");
        assert!(failure.reason.contains("`cargo test` failed"), "{}", failure.reason);
        assert!(failure.output.ends_with("2 failed\nTOKEN=[REDACTED]"), "{}", failure.output);
        assert!(failure.output.len() <= 1600, "only the tail is kept: {}", failure.output.len());
        assert!(!failure.output.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), "secrets are redacted: {}", failure.output);
        assert!(!failure.reason.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), "secrets are redacted: {}", failure.reason);
    }
}

#[test]
fn tests_that_change_the_worktree_are_recorded_as_a_failed_accept() {
    let (_repo, _worktree, store) = ended_run("mutated", "failed");
    accept_run_by(&store, "run-1", "web", &|dir: &Path| {
        std::fs::write(dir.join("generated.txt"), "left by the tests").unwrap();
        Ok(())
    })
    .unwrap_err();
    let summary = summarize_run(&store.events("run-1").unwrap());
    assert_eq!(summary.phase, RunPhase::Failed);
    assert!(summary.accept_failure.unwrap().reason.contains("changed the worktree"));
}

#[test]
fn an_accept_while_another_is_running_is_refused_saying_so() {
    let (_repo, _worktree, store) = ended_run("twice", "failed");
    store.append_event(&event("run.accept_started", serde_json::json!({ "by": "web", "head": "abc" }))).unwrap();
    let err = orq_orchestrator::accept_check(&store, "run-1").unwrap_err();
    assert!(err.contains("an accept is running"), "{err}");
}

#[test]
fn a_refused_accept_records_nothing() {
    let (_repo, worktree, store) = ended_run("refused", "failed");
    std::fs::write(worktree.join("dirty.txt"), "uncommitted").unwrap();
    let before = kinds(&store);
    accept_run_by(&store, "run-1", "web", &|_: &Path| Ok(())).unwrap_err();
    assert_eq!(kinds(&store), before);
}

#[test]
fn a_started_accept_with_no_result_reads_accepting_and_a_new_accept_clears_the_last_failure() {
    let events = vec![
        event("run.completed", serde_json::json!({ "ending": "failed", "reason": "gate failed" })),
        event("run.accept_started", serde_json::json!({ "by": "web", "head": "abc" })),
        event("run.accept_failed", serde_json::json!({ "reason": "tests failed", "output": "boom" })),
        Event { occurred_at: "2026-11-02T10:05:00Z".into(), ..event("run.accept_started", serde_json::json!({ "by": "web", "head": "abc" })) },
    ];
    let summary = summarize_run(&events);
    assert_eq!(summary.phase, RunPhase::Accepting);
    assert_eq!(RunPhase::Accepting.as_str(), "accepting");
    assert_eq!(summary.phase_at.as_deref(), Some("2026-11-02T10:05:00Z"), "since when it is accepting");
    assert_eq!(summary.accept_failure, None);
}

#[test]
fn an_accept_the_server_stopped_during_is_recorded_as_failed_and_a_settled_run_is_left_alone() {
    let (_repo, _worktree, store) = ended_run("interrupted", "needs_review");
    store.append_event(&event("run.accept_started", serde_json::json!({ "by": "web", "head": "abc" }))).unwrap();

    assert_eq!(fail_interrupted_accept(&store, "run-1"), Ok(true));

    let summary = summarize_run(&store.events("run-1").unwrap());
    assert_eq!(summary.phase, RunPhase::NeedsReview);
    assert_eq!(summary.accept_failure.unwrap().reason, "the server stopped while the tests were running; accept again");
    let before = kinds(&store);
    assert_eq!(fail_interrupted_accept(&store, "run-1"), Ok(false));
    assert_eq!(kinds(&store), before);
}

const TOKEN: &str = "ghp_SECRETSECRETSECRETSECRET0123456789AB";

fn recorded_failure(tag: &str, why: String) -> orq_orchestrator::AcceptFailure {
    let (_repo, _worktree, store) = ended_run(tag, "failed");
    accept_run_by(&store, "run-1", "web", &|_: &Path| Err(why.clone())).unwrap_err();
    summarize_run(&store.events("run-1").unwrap()).accept_failure.expect("the accept failure")
}

#[test]
fn a_secret_cut_by_the_reason_limit_is_redacted_whole_first() {
    // The first line is cut near 300 characters, inside the token.
    let failure = recorded_failure("cut-reason", format!("{} {TOKEN}", "x".repeat(254)));
    assert!(!failure.reason.contains("SECRETSECRET"), "{}", failure.reason);
}

#[test]
fn a_secret_cut_by_the_output_tail_is_redacted_whole_first() {
    // The kept tail starts inside the token: only its end would be left, too short to be recognised.
    let why = format!("`cargo test` failed: noise\n{TOKEN}\n{}", "z".repeat(1472));
    let failure = recorded_failure("cut-output", why);
    assert!(!failure.output.contains("0123456789AB"), "{}", failure.output);
}

#[test]
fn a_private_key_whose_start_is_cut_off_is_not_kept() {
    let key = format!("-----BEGIN RSA PRIVATE KEY-----\n{}-----END RSA PRIVATE KEY-----", "MIIkeybodylineMIIkeybodyline\n".repeat(200));
    let failure = recorded_failure("cut-key", format!("`cargo test` failed: {key}\ntest result: FAILED"));
    assert!(!failure.output.contains("MIIkeybodyline"), "{}", failure.output);
}

#[test]
fn test_output_is_redacted_before_only_its_tail_is_kept() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-accept-steps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = format!("printf 'start\\n{TOKEN}\\n'; head -c 1473 /dev/zero | tr '\\0' z; exit 1");
    let err = orq_orchestrator::run_steps(&dir, &[vec!["sh".into(), "-c".into(), script]]).unwrap_err();
    // What the command printed, after the command itself.
    let err = err.rsplit_once("` failed: ").unwrap().1;
    assert!(err.contains("zzz"), "{err}");
    assert!(!err.contains("0123456789AB"), "{err}");
}

#[test]
fn a_private_key_whose_start_scrolled_out_of_the_kept_output_is_not_kept() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-accept-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Far more than is kept, so its BEGIN line is gone before anything can recognise it.
    let script = "echo '-----BEGIN RSA PRIVATE KEY-----'; i=0; while [ $i -lt 400 ]; do echo MIIkeybodylineMIIkeybodyline; i=$((i+1)); done; echo '-----END RSA PRIVATE KEY-----'; echo 'test result: FAILED'; exit 1";
    let err = orq_orchestrator::run_steps(&dir, &[vec!["sh".into(), "-c".into(), script.into()]]).unwrap_err();
    let err = err.rsplit_once("` failed: ").unwrap().1;
    assert!(err.contains("test result: FAILED"), "{err}");
    assert!(!err.contains("MIIkeybodyline"), "{err}");
}

#[test]
fn tests_that_panic_are_recorded_as_a_failed_accept() {
    let (_repo, _worktree, store) = ended_run("panic", "needs_review");
    let err = accept_run_by(&store, "run-1", "web", &|_: &Path| panic!("the test runner blew up")).unwrap_err();
    assert!(err.contains("the test runner blew up"), "{err}");
    let summary = summarize_run(&store.events("run-1").unwrap());
    assert_eq!(summary.phase, RunPhase::NeedsReview, "not left accepting");
    assert!(summary.accept_failure.unwrap().reason.contains("the test runner blew up"));
}
