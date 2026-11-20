//! A run whose implement step finished but whose review stopped it (escalated with findings open) is
//! resumed with a session, not as finished; a resume that would skip the session refuses an answer
//! instead of dropping it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{
    check_answer_reaches_session, plan_resume, supersede_resumed_with_session, Event, RunRecord,
    RunStore,
};
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

struct World {
    store: RunStore,
    repo: PathBuf,
    worktree: PathBuf,
    record: RunRecord,
    /// The head the review looked at.
    reviewed: String,
}

fn event(record: &RunRecord, n: u32, kind: &str, stage: Option<&str>, payload: Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("{}-{n}", record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: stage.map(str::to_string),
        attempt: 1,
        occurred_at: format!("2026-11-20T10:00:{n:02}Z"),
        payload,
    }
}

/// A run whose implement step completed (its work committed and reviewed with one open finding at
/// the escalation) and that then ended `ending` for `reason`.
fn world(tag: &str, ending: &str, reason: &str, review_stopped: bool) -> World {
    let root = orq_test_support::process_temp_dir().join(format!(
        "orq-resume-escalation-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "a.txt", "base");
    let worktree = repo.join(".claude/worktrees/fix--F");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fix--F",
            worktree.to_str().unwrap(),
        ],
    );
    let base = git(&repo, &["rev-parse", "main"]);
    let reviewed = commit(&worktree, "work.txt", "the work");
    if review_stopped {
        let scope = format!("review:{base}");
        let finding = json!({
            "id": "leak", "severity": "high", "file": "src/lib.rs", "line": 7, "category": "bugs",
            "claim": "claim of leak", "suggestion": "fix it", "status": "open", "reviewer": "pi", "model": "m",
        });
        let round = json!({
            "start": base, "head": reviewed, "reviewer": "pi", "model": "m", "findings": [finding.clone()],
            "round": 3, "recordedAt": "2026-11-20T09:00:00+00:00",
        });
        fs::write(
            worktree.join(".tdd-review-rounds.json"),
            json!({ &scope: [round] }).to_string(),
        )
        .unwrap();
        fs::write(
            worktree.join(".tdd-review-findings.json"),
            json!({ &scope: [finding] }).to_string(),
        )
        .unwrap();
    }
    let store = RunStore::new(&root.join("state"));
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "F".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: base,
        branch: "fix--F".to_string(),
        worktree: worktree.clone(),
        repository: "https://github.example/o/r".to_string(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    store
        .append_event(&event(&record, 1, "run.started", None, json!({})))
        .unwrap();
    store
        .append_event(&event(
            &record,
            2,
            "stage.started",
            Some("implement_and_review"),
            json!({}),
        ))
        .unwrap();
    store
        .append_event(&event(
            &record,
            3,
            "stage.completed",
            Some("implement_and_review"),
            json!({}),
        ))
        .unwrap();
    store
        .append_event(&event(
            &record,
            4,
            "run.completed",
            None,
            json!({ "ending": ending, "reason": reason }),
        ))
        .unwrap();
    World {
        store,
        repo,
        worktree,
        record,
        reviewed,
    }
}

const ESCALATED: &str = "the review escalated after 3 rounds with 1 finding open, and an unattended run does not leave a high-severity or security finding for later: leak (high, bugs)";

#[test]
fn resume_after_escalation_a_run_blocked_by_its_escalated_review_is_resumed_with_a_session_that_reviews_what_came_after_the_escalated_head(
) {
    let w = world("escalated", "blocked", ESCALATED, true);
    let session_commit = commit(&w.worktree, "fix.txt", "the session's fix");

    let plan = plan_resume(&w.store, &w.repo, "repo-1", "F").expect("a blocked run resumes");

    assert!(!plan.skip_implement, "the session runs again");
    let review = plan
        .review_again
        .as_ref()
        .expect("the review is asked for again");
    assert_eq!(review.reviewed_head, w.reviewed);
    let unreviewed: Vec<&str> = review
        .unreviewed_commits
        .iter()
        .map(|c| c.sha.as_str())
        .collect();
    assert_eq!(
        unreviewed,
        vec![session_commit.as_str()],
        "only commits after the escalated head"
    );
    // An answer reaches the session, so it is not refused.
    check_answer_reaches_session(&plan, Some("rename the helper"))
        .expect("the answer reaches the session");
}

#[test]
fn resume_after_escalation_with_no_new_commits_the_escalated_findings_are_still_the_ones_to_review_again(
) {
    let w = world("no-new", "blocked", ESCALATED, true);

    let plan = plan_resume(&w.store, &w.repo, "repo-1", "F").unwrap();

    assert!(!plan.skip_implement);
    let review = plan
        .review_again
        .as_ref()
        .expect("the review is asked for again");
    assert!(review.unreviewed_commits.is_empty());
    let open: Vec<&str> = review
        .findings
        .iter()
        .filter(|f| f.status == "open")
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(open, vec!["leak"], "the same open findings are named");
}

#[test]
fn resume_after_escalation_a_gate_failure_after_a_clean_review_still_skips_the_session_and_refuses_an_answer(
) {
    let w = world("gate", "failed", "the gate failed", false);

    let plan = plan_resume(&w.store, &w.repo, "repo-1", "F").unwrap();

    assert!(plan.skip_implement);
    assert!(plan.review_again.is_none());
    let refused = check_answer_reaches_session(&plan, Some("please fix it")).unwrap_err();
    assert_eq!(
        refused,
        "this run's session already finished cleanly; an answer cannot reach it"
    );
    check_answer_reaches_session(&plan, None).expect("no answer, nothing to drop");
    check_answer_reaches_session(&plan, Some("  ")).expect("a blank answer says nothing");
}

#[test]
fn resume_after_escalation_run_resumed_records_whether_the_session_was_started_or_inherited_and_why(
) {
    let w = world("recorded", "blocked", ESCALATED, true);
    let plan = plan_resume(&w.store, &w.repo, "repo-1", "F").unwrap();
    let (session, why) = plan.session();
    assert_eq!(session, "started");
    assert!(why.contains("review"), "{why}");
    supersede_resumed_with_session(&w.store, &plan.from, "run-2", session, &why).unwrap();

    let gate = world("recorded-gate", "failed", "the gate failed", false);
    let plan = plan_resume(&gate.store, &gate.repo, "repo-1", "F").unwrap();
    let (session, why) = plan.session();
    assert_eq!(session, "inherited");
    supersede_resumed_with_session(&gate.store, &plan.from, "run-2", session, &why).unwrap();

    let resumed = |store: &RunStore| {
        store
            .events("run-1")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "run.resumed")
            .expect("run.resumed")
            .payload
    };
    let started = resumed(&w.store);
    assert_eq!(started["resumed_as"], "run-2");
    assert_eq!(started["session"], "started");
    assert!(started["why"].as_str().unwrap().contains("review"));
    let inherited = resumed(&gate.store);
    assert_eq!(inherited["session"], "inherited");
    assert!(!inherited["why"].as_str().unwrap().is_empty());
    let _ = &w.record;
}

#[test]
fn resume_after_escalation_a_session_problem_resumes_even_when_the_review_ledger_is_unreadable() {
    let w = world("unreadable-ledger", "failed", "the implement session reported a problem: the tests hang", false);
    fs::write(w.worktree.join(".tdd-review-rounds.json"), "not json").unwrap();

    let plan = plan_resume(&w.store, &w.repo, "repo-1", "F").expect("an unrelated session problem still resumes");

    assert!(!plan.skip_implement);
    assert!(plan.review_again.is_none());
}
