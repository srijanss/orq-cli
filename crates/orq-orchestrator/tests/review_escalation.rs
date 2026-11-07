//! A run whose implement session stopped because its review loop escalated: orq reads the
//! escalation off the worktree's review ledger, so the run waits for a human decision instead of
//! failing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::escalated_review;
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, message: &str) -> String {
    fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A repository with a base commit; the run's work goes on top of it.
fn repo(tag: &str) -> (PathBuf, String) {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-review-escalation-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base.txt", "base");
    (dir, base)
}

fn finding(id: &str, severity: &str, status: &str) -> Value {
    json!({
        "id": id, "severity": severity, "file": "src/lib.rs", "line": 7, "category": "bugs",
        "claim": format!("claim of {id}"), "suggestion": "fix it", "status": status,
        "reviewer": "pi", "model": "m",
    })
}

fn round(start: &str, head: &str, number: u32, findings: Vec<Value>) -> Value {
    json!({
        "start": start, "head": head, "reviewer": "pi", "model": "m", "findings": findings,
        "round": number, "recordedAt": format!("2026-11-07T12:0{number}:00+00:00"),
    })
}

fn ledger(dir: &Path, start: &str, rounds: Vec<Value>, live: Vec<Value>) {
    let scope = format!("review:{start}");
    fs::write(dir.join(".tdd-review-rounds.json"), json!({ &scope: rounds }).to_string()).unwrap();
    fs::write(dir.join(".tdd-review-findings.json"), json!({ &scope: live }).to_string()).unwrap();
}

#[test]
fn a_review_that_escalated_at_the_round_cap_names_its_open_findings_and_the_commits_no_reviewer_saw() {
    let (dir, base) = repo("cap");
    let first = commit(&dir, "a.txt", "feature");
    let second = commit(&dir, "b.txt", "fix round one");
    let reviewed = commit(&dir, "c.txt", "fix round two");
    ledger(
        &dir,
        &base,
        vec![
            round(&base, &first, 1, vec![finding("one", "high", "open")]),
            round(&base, &second, 2, vec![finding("two", "medium", "open")]),
            round(&base, &reviewed, 3, vec![finding("still-there", "high", "open"), finding("fixed-by-hand", "medium", "open"), finding("minor", "low", "open")]),
        ],
        // The human fixed one after the escalation; the other is still open.
        vec![finding("still-there", "high", "open"), finding("minor", "low", "open")],
    );
    let unseen = commit(&dir, "d.txt", "fix by hand");

    let review = escalated_review(&dir, &base).unwrap().expect("the review escalated");

    assert_eq!(review.start, base);
    assert_eq!(review.reviewed_head, reviewed);
    assert_eq!(review.rounds, 3);
    let findings: Vec<(&str, &str, &str)> =
        review.findings.iter().map(|f| (f.id.as_str(), f.severity.as_str(), f.status.as_str())).collect();
    assert_eq!(findings, vec![("still-there", "high", "open"), ("fixed-by-hand", "medium", "fixed")]);
    assert_eq!(review.findings[0].claim, "claim of still-there");
    let commits: Vec<&str> = review.unreviewed_commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(commits, vec![unseen.as_str()]);
    assert_eq!(review.unreviewed_commits[0].subject, "fix by hand");
}

/// A run of `repo`'s linked worktree, recorded in a fresh store; `base` is where its branch starts.
fn stored_run(tag: &str) -> (orq_test_support::GitFixture, PathBuf, orq_orchestrator::RunStore, orq_orchestrator::RunRecord) {
    let repo = orq_test_support::GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    commit(&worktree, "work.txt", "work");
    let root = orq_test_support::process_temp_dir().join(format!("orq-review-escalation-store-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = orq_orchestrator::RunStore::new(&root);
    let record = orq_orchestrator::RunRecord {
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
        profiles: Default::default(),
    };
    store.start_run(&record).unwrap();
    (repo, worktree, store, record)
}

fn summary(store: &orq_orchestrator::RunStore) -> orq_orchestrator::RunSummary {
    orq_orchestrator::summarize_run(&store.events("run-1").unwrap())
}

#[test]
fn a_run_ended_for_a_review_decision_waits_for_one_and_can_be_accepted() {
    use orq_orchestrator::{accept_run, record_run_completed, RunPhase};
    let (_repo, worktree, store, record) = stored_run("accept");
    record_run_completed(&store, &record, "needs_review", Some("the review escalated after 3 rounds")).unwrap();

    let waiting = summary(&store);
    assert_eq!(waiting.phase, RunPhase::NeedsReview);
    assert_eq!(waiting.phase.as_str(), "needs_review");
    assert_eq!(waiting.reason.as_deref(), Some("the review escalated after 3 rounds"));

    let tested = std::cell::RefCell::new(None);
    accept_run(&store, "run-1", &|dir: &Path| {
        *tested.borrow_mut() = Some(dir.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(tested.into_inner(), Some(worktree));
    assert_eq!(summary(&store).phase, RunPhase::Completed);
}

#[test]
fn a_newer_review_that_is_not_of_this_run_does_not_hide_the_one_that_escalated() {
    let (dir, base) = repo("other-scopes");
    let first = commit(&dir, "a.txt", "feature");
    let reviewed = commit(&dir, "b.txt", "fix");
    let later = |mut round: Value| {
        round["recordedAt"] = json!("2026-11-07T13:00:00+00:00");
        round
    };
    let scopes = json!({
        format!("review:{base}"): [round(&base, &reviewed, 3, vec![finding("leak", "high", "open")])],
        // A review of commits this repository does not have.
        "review:abcdef0123": [later(round("abcdef0123", "abcdef4567", 1, vec![]))],
        // A review whose head does not come after its start.
        format!("review:{reviewed}"): [later(round(&reviewed, &first, 1, vec![]))],
    });
    fs::write(dir.join(".tdd-review-rounds.json"), scopes.to_string()).unwrap();
    fs::write(dir.join(".tdd-review-findings.json"), json!({ format!("review:{base}"): [finding("leak", "high", "open")] }).to_string()).unwrap();

    let review = escalated_review(&dir, &base).unwrap().expect("the run's review escalated");

    assert_eq!(review.start, base);
    assert_eq!(review.reviewed_head, reviewed);
}

#[test]
fn a_newer_round_whose_head_is_not_after_its_start_is_not_taken_for_the_latest_review() {
    let (dir, base) = repo("head-before-start");
    let first = commit(&dir, "a.txt", "feature");
    let reviewed = commit(&dir, "b.txt", "fix");
    let mut backwards = round(&reviewed, &first, 1, vec![]);
    backwards["recordedAt"] = json!("2026-11-07T13:00:00+00:00");
    let scopes = json!({
        format!("review:{base}"): [round(&base, &reviewed, 3, vec![finding("leak", "high", "open")])],
        format!("review:{reviewed}"): [backwards],
    });
    fs::write(dir.join(".tdd-review-rounds.json"), scopes.to_string()).unwrap();

    let review = escalated_review(&dir, &base).unwrap().expect("the run's review escalated");

    assert_eq!(review.reviewed_head, reviewed);
}

#[test]
fn a_finding_listed_twice_in_the_live_ledger_takes_the_status_of_its_first_entry() {
    let (dir, base) = repo("duplicate-live");
    let reviewed = commit(&dir, "a.txt", "feature");
    ledger(
        &dir,
        &base,
        vec![round(&base, &reviewed, 3, vec![finding("leak", "high", "open"), finding("race", "medium", "open")])],
        vec![finding("race", "medium", "deferred"), finding("leak", "high", "rejected"), finding("leak", "high", "open")],
    );

    let review = escalated_review(&dir, &base).unwrap().expect("the review escalated");

    let statuses: Vec<(&str, &str)> = review.findings.iter().map(|f| (f.id.as_str(), f.status.as_str())).collect();
    assert_eq!(statuses, vec![("leak", "rejected"), ("race", "deferred")]);
}
