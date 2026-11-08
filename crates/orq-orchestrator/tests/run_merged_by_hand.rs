use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{discard_run_then, list_runs_report, mark_merged_runs, Event, RunPhase, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "--quiet", "-m", file]);
}

fn event(run_id: &str, kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{run_id}-{kind}"),
        run_id: run_id.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

/// A repository with a run worktree on `orq-run-1`, started from the target branch's tip.
struct Setup {
    repo: GitFixture,
    worktree: PathBuf,
    target: String,
    base: String,
    root: PathBuf,
    store: RunStore,
}

fn setup(tag: &str) -> Setup {
    let repo = GitFixture::new();
    let target = git(repo.root(), &["symbolic-ref", "--short", "HEAD"]);
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    let root = orq_test_support::process_temp_dir().join(format!("orq-merged-by-hand-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    Setup { repo, worktree, target, base, root, store }
}

impl Setup {
    fn record(&self, run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: self.target.clone(),
            base_sha: self.base.clone(),
            branch: "orq-run-1".into(),
            worktree: self.worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        }
    }

    /// Start `run-1` and end it with `ending` (`failed` or `needs_review`).
    fn ended_run(&self, ending: &str) {
        self.store.start_run(&self.record("run-1")).unwrap();
        self.store.append_event(&event("run-1", "run.completed", serde_json::json!({ "ending": ending }))).unwrap();
    }

    fn merge_by_hand(&self) {
        git(self.repo.root(), &["merge", "--quiet", "--no-ff", "-m", "merge", "orq-run-1"]);
    }

    fn listed_phase(&self) -> RunPhase {
        let mut list = list_runs_report(&self.store, "p").unwrap();
        mark_merged_runs(&self.store, &mut list, self.repo.root());
        list.runs.iter().find(|run| run.run_id == "run-1").unwrap().phase
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_failed_run_whose_commits_are_all_on_the_target_branch_is_listed_as_merged() {
    let s = setup("failed-merged");
    commit(&s.worktree, "a.txt");
    commit(&s.worktree, "b.txt");
    s.ended_run("failed");
    s.merge_by_hand();

    assert_eq!(s.listed_phase(), RunPhase::Merged);
    assert_eq!(RunPhase::Merged.as_str(), "merged");
}

#[test]
fn a_run_with_a_commit_not_on_the_target_or_with_no_commits_is_not_merged_but_a_merged_review_waiting_run_is() {
    let s = setup("partly-merged");
    commit(&s.worktree, "a.txt");
    s.ended_run("needs_review");
    assert_eq!(s.listed_phase(), RunPhase::NeedsReview, "a branch not merged at all");
    s.merge_by_hand();
    assert_eq!(s.listed_phase(), RunPhase::Merged, "a review-waiting run merged by hand");
    commit(&s.worktree, "b.txt");
    assert_eq!(s.listed_phase(), RunPhase::NeedsReview, "one commit past what was merged");

    let empty = setup("no-commits");
    empty.ended_run("failed");
    assert_eq!(empty.listed_phase(), RunPhase::Failed, "a run with no commits");
}

#[test]
fn discarding_a_merged_run_removes_its_record_logs_and_locks_without_reopening_its_features() {
    let s = setup("discard-merged");
    commit(&s.worktree, "a.txt");
    s.ended_run("failed");
    s.merge_by_hand();
    std::fs::create_dir_all(s.root.join("logs").join("run-1")).unwrap();

    let mut reopened = Vec::new();
    let outcome = discard_run_then(&s.store, "run-1", s.repo.root(), &mut |_, features| reopened.extend_from_slice(features)).unwrap();

    assert!(outcome.merged);
    assert!(reopened.is_empty(), "features handed back to reopen: {reopened:?}");
    assert!(s.store.load("run-1").is_err());
    assert!(!s.root.join("logs").join("run-1").exists());
    assert_eq!(s.store.active_run("p", "F").unwrap(), None);
}

#[test]
fn discarding_a_failed_run_whose_work_is_not_on_the_target_still_reopens_its_features() {
    let s = setup("discard-unmerged");
    commit(&s.worktree, "a.txt");
    s.ended_run("failed");

    let mut reopened = Vec::new();
    let outcome = discard_run_then(&s.store, "run-1", s.repo.root(), &mut |_, features| reopened.extend_from_slice(features)).unwrap();

    assert!(!outcome.merged);
    assert_eq!(reopened, vec!["F".to_string()]);
}
