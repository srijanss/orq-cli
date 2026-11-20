//! A run whose implement session stopped because its review loop escalated: orq reads the
//! escalation off the worktree's review ledger, so the run waits for a human decision instead of
//! failing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{escalated_review, last_review};
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let dir = orq_test_support::process_temp_dir().join(format!(
        "orq-review-escalation-{}-{tag}",
        std::process::id()
    ));
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
    fs::write(
        dir.join(".tdd-review-rounds.json"),
        json!({ &scope: rounds }).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join(".tdd-review-findings.json"),
        json!({ &scope: live }).to_string(),
    )
    .unwrap();
}

#[test]
fn a_review_that_escalated_at_the_round_cap_names_its_open_findings_and_the_commits_no_reviewer_saw(
) {
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
            round(
                &base,
                &reviewed,
                3,
                vec![
                    finding("still-there", "high", "open"),
                    finding("fixed-by-hand", "medium", "open"),
                    finding("minor", "low", "open"),
                ],
            ),
        ],
        // The human fixed one after the escalation; the other is still open.
        vec![
            finding("still-there", "high", "open"),
            finding("minor", "low", "open"),
        ],
    );
    let unseen = commit(&dir, "d.txt", "fix by hand");

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the review escalated");

    assert_eq!(review.start, base);
    assert_eq!(review.reviewed_head, reviewed);
    assert_eq!(review.rounds, 3);
    let findings: Vec<(&str, &str, &str)> = review
        .findings
        .iter()
        .map(|f| (f.id.as_str(), f.severity.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(
        findings,
        vec![
            ("still-there", "high", "open"),
            ("fixed-by-hand", "medium", "fixed")
        ]
    );
    assert_eq!(review.findings[0].claim, "claim of still-there");
    let commits: Vec<&str> = review
        .unreviewed_commits
        .iter()
        .map(|c| c.sha.as_str())
        .collect();
    assert_eq!(commits, vec![unseen.as_str()]);
    assert_eq!(review.unreviewed_commits[0].subject, "fix by hand");
}

/// An unattended run turns each open finding into a draft follow-up, which says what the reviewer
/// found and suggested and in which category.
#[test]
fn an_escalated_finding_carries_its_category_and_suggestion() {
    let (dir, base) = repo("category");
    let reviewed = commit(&dir, "a.txt", "feature");
    let mut leak = finding("leak", "medium", "open");
    leak["category"] = json!("security");
    let mut bare = finding("bare", "medium", "open");
    bare.as_object_mut().unwrap().remove("category");
    bare.as_object_mut().unwrap().remove("suggestion");
    ledger(
        &dir,
        &base,
        vec![round(&base, &reviewed, 3, vec![leak.clone(), bare.clone()])],
        vec![leak, bare],
    );

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the review escalated");

    assert_eq!(review.findings[0].category.as_deref(), Some("security"));
    assert_eq!(review.findings[0].suggestion.as_deref(), Some("fix it"));
    assert_eq!(review.findings[1].category, None);
    assert_eq!(review.findings[1].suggestion, None);
}

/// A run of `repo`'s linked worktree, recorded in a fresh store; `base` is where its branch starts.
fn stored_run(
    tag: &str,
) -> (
    orq_test_support::GitFixture,
    PathBuf,
    orq_orchestrator::RunStore,
    orq_orchestrator::RunRecord,
) {
    let repo = orq_test_support::GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    commit(&worktree, "work.txt", "work");
    let root = orq_test_support::process_temp_dir().join(format!(
        "orq-review-escalation-store-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = orq_orchestrator::RunStore::new(&root);
    let record = orq_orchestrator::RunRecord {
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
    record_run_completed(
        &store,
        &record,
        "needs_review",
        Some("the review escalated after 3 rounds"),
    )
    .unwrap();

    let waiting = summary(&store);
    assert_eq!(waiting.phase, RunPhase::NeedsReview);
    assert_eq!(waiting.phase.as_str(), "needs_review");
    assert_eq!(
        waiting.reason.as_deref(),
        Some("the review escalated after 3 rounds")
    );

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
    fs::write(
        dir.join(".tdd-review-findings.json"),
        json!({ format!("review:{base}"): [finding("leak", "high", "open")] }).to_string(),
    )
    .unwrap();

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the run's review escalated");

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

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the run's review escalated");

    assert_eq!(review.reviewed_head, reviewed);
}

#[test]
fn a_finding_listed_twice_in_the_live_ledger_takes_the_status_of_its_first_entry() {
    let (dir, base) = repo("duplicate-live");
    let reviewed = commit(&dir, "a.txt", "feature");
    ledger(
        &dir,
        &base,
        vec![round(
            &base,
            &reviewed,
            3,
            vec![
                finding("leak", "high", "open"),
                finding("race", "medium", "open"),
            ],
        )],
        vec![
            finding("race", "medium", "deferred"),
            finding("leak", "high", "rejected"),
            finding("leak", "high", "open"),
        ],
    );

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the review escalated");

    let statuses: Vec<(&str, &str)> = review
        .findings
        .iter()
        .map(|f| (f.id.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(statuses, vec![("leak", "rejected"), ("race", "deferred")]);
}

#[test]
fn a_review_whose_ledger_names_its_commits_abbreviated_is_still_the_one_that_escalated() {
    let (dir, base) = repo("abbreviated");
    let reviewed = commit(&dir, "a.txt", "feature");
    let (short_base, short_reviewed) = (&base[..7], &reviewed[..9]);
    ledger(
        &dir,
        short_base,
        vec![round(
            short_base,
            short_reviewed,
            3,
            vec![finding("leak", "high", "open")],
        )],
        vec![finding("leak", "high", "deferred")],
    );

    let review = escalated_review(&dir, &base)
        .unwrap()
        .expect("the review escalated");

    assert_eq!(review.start, base);
    assert_eq!(review.reviewed_head, reviewed);
    let statuses: Vec<(&str, &str)> = review
        .findings
        .iter()
        .map(|f| (f.id.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(statuses, vec![("leak", "deferred")]);
}

/// `escalated_review` of `dir`, given at most a few seconds: a ledger that would block the read
/// fails the test instead of hanging it.
fn escalated_review_in_time(
    dir: &Path,
    base: &str,
) -> Result<Option<orq_orchestrator::EscalatedReview>, String> {
    let (dir, base) = (dir.to_path_buf(), base.to_string());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(escalated_review(&dir, &base));
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("reading the ledger does not block")
}

#[test]
fn a_ledger_that_is_not_a_plain_readable_json_file_is_an_error_and_a_missing_one_is_no_review() {
    let (dir, base) = repo("ledger-kinds");
    let reviewed = commit(&dir, "a.txt", "feature");
    let rounds = dir.join(".tdd-review-rounds.json");
    let findings = dir.join(".tdd-review-findings.json");
    let escalated = json!({ format!("review:{base}"): [round(&base, &reviewed, 3, vec![finding("leak", "high", "open")])] }).to_string();
    let elsewhere = dir.join("elsewhere.json");
    fs::write(&elsewhere, &escalated).unwrap();

    assert_eq!(
        escalated_review_in_time(&dir, &base).unwrap(),
        None,
        "no ledger, no review"
    );

    std::os::unix::fs::symlink(&elsewhere, &rounds).unwrap();
    let err = escalated_review_in_time(&dir, &base).unwrap_err();
    assert!(
        err.contains(".tdd-review-rounds.json"),
        "a symlinked ledger is refused: {err}"
    );
    fs::remove_file(&rounds).unwrap();

    let made = Command::new("mkfifo").arg(&rounds).status().unwrap();
    assert!(made.success());
    let err = escalated_review_in_time(&dir, &base).unwrap_err();
    assert!(
        err.contains("not a regular file"),
        "a FIFO ledger is refused without waiting on a writer: {err}"
    );
    fs::remove_file(&rounds).unwrap();

    let mut oversized = escalated.clone().into_bytes();
    oversized.resize(4 * 1024 * 1024 + 1, b' ');
    fs::write(&rounds, oversized).unwrap();
    let err = escalated_review_in_time(&dir, &base).unwrap_err();
    assert!(
        err.contains("larger than"),
        "an oversized ledger is refused: {err}"
    );

    fs::write(&rounds, "{ not json").unwrap();
    let err = escalated_review_in_time(&dir, &base).unwrap_err();
    assert!(
        err.contains("not valid JSON"),
        "a malformed ledger is refused: {err}"
    );

    fs::write(&rounds, &escalated).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &findings).unwrap();
    let err = escalated_review_in_time(&dir, &base).unwrap_err();
    assert!(
        err.contains(".tdd-review-findings.json"),
        "a symlinked findings ledger is refused too: {err}"
    );
}

#[test]
fn the_last_review_of_a_run_is_read_whether_or_not_it_escalated() {
    let (dir, base) = repo("last-review");
    let reviewed = commit(&dir, "a.txt", "feature");
    ledger(
        &dir,
        &base,
        vec![round(
            &base,
            &reviewed,
            1,
            vec![
                finding("open-one", "high", "open"),
                finding("minor", "low", "open"),
            ],
        )],
        vec![finding("open-one", "high", "open")],
    );
    let unseen = commit(&dir, "b.txt", "fix after the review");

    assert_eq!(
        escalated_review(&dir, &base).unwrap(),
        None,
        "one round with a blocking finding did not escalate"
    );
    let review = last_review(&dir, &base)
        .unwrap()
        .expect("the run's last review");
    assert_eq!(
        (review.reviewed_head.as_str(), review.rounds),
        (reviewed.as_str(), 1)
    );
    let findings: Vec<(&str, &str)> = review
        .findings
        .iter()
        .map(|f| (f.id.as_str(), f.status.as_str()))
        .collect();
    assert_eq!(findings, vec![("open-one", "open")]);
    let commits: Vec<&str> = review
        .unreviewed_commits
        .iter()
        .map(|c| c.sha.as_str())
        .collect();
    assert_eq!(commits, vec![unseen.as_str()]);

    let (empty, empty_base) = repo("last-review-none");
    assert_eq!(
        last_review(&empty, &empty_base).unwrap(),
        None,
        "no ledger is no review"
    );
}

/// [`stored_run`], with its worktree where orq keeps a run's, as a resume plan requires.
fn resumable_run(
    tag: &str,
) -> (
    orq_test_support::GitFixture,
    PathBuf,
    orq_orchestrator::RunStore,
    orq_orchestrator::RunRecord,
) {
    let repo = orq_test_support::GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.root().join(".claude/worktrees/run-1");
    git(
        repo.root(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "orq/run-1",
            worktree.to_str().unwrap(),
        ],
    );
    commit(&worktree, "work.txt", "work");
    let root = orq_test_support::process_temp_dir().join(format!(
        "orq-review-again-store-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = orq_orchestrator::RunStore::new(&root);
    let record = orq_orchestrator::RunRecord {
        delivery: Default::default(),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: base,
        branch: "orq/run-1".into(),
        worktree: worktree.clone(),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.start_run(&record).unwrap();
    (repo, worktree, store, record)
}

#[test]
fn a_review_again_asked_of_a_failed_run_starts_from_its_last_review_when_something_came_after_it() {
    use orq_orchestrator::{plan_group_resume_reviewing_again, record_run_completed};
    let members = ["F".to_string()];
    let (repo, worktree, store, record) = resumable_run("again-failed");
    let reviewed = git(&worktree, &["rev-parse", "HEAD"]);
    ledger(
        &worktree,
        &record.base_sha,
        vec![round(
            &record.base_sha,
            &reviewed,
            1,
            vec![finding("open-one", "high", "open")],
        )],
        vec![finding("open-one", "high", "open")],
    );
    record_run_completed(
        &store,
        &record,
        "failed",
        Some("the implement session reported a problem"),
    )
    .unwrap();

    let nothing_new =
        plan_group_resume_reviewing_again(&store, repo.root(), "p", &members).unwrap_err();
    assert_eq!(
        nothing_new,
        "run-1: nothing was committed after its last review; resume it instead"
    );

    commit(&worktree, "later.txt", "fix after the review");
    let plan = plan_group_resume_reviewing_again(&store, repo.root(), "p", &members).unwrap();
    let again = plan.review_again.expect("a review again");
    assert_eq!(again.reviewed_head, reviewed);
    assert!(!plan.skip_implement, "the session reviews again");
    assert!(
        orq_orchestrator::plan_group_resume(&store, repo.root(), "p", &members)
            .unwrap()
            .review_again
            .is_none(),
        "a plain resume of a failed run is not a review again"
    );
}

#[test]
fn a_review_again_is_refused_for_a_run_that_neither_failed_nor_needs_review_or_that_was_never_reviewed(
) {
    use orq_orchestrator::{plan_group_resume_reviewing_again, record_run_completed};
    let members = ["F".to_string()];
    let (repo, _worktree, store, record) = resumable_run("again-blocked");
    record_run_completed(&store, &record, "blocked", Some("rebase conflict")).unwrap();
    assert_eq!(
        plan_group_resume_reviewing_again(&store, repo.root(), "p", &members).unwrap_err(),
        "run-1 is blocked: only a run that needs review or failed can be reviewed again"
    );

    let (repo, _worktree, store, record) = resumable_run("again-unreviewed");
    record_run_completed(&store, &record, "failed", None).unwrap();
    assert_eq!(
        plan_group_resume_reviewing_again(&store, repo.root(), "p", &members).unwrap_err(),
        "run-1: its worktree has no review to review again; resume it instead"
    );
}
