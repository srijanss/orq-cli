//! Accepting a merge-local run carries it on to the merge: when its tests pass, its branch is caught up
//! with the target, tested again if that changed it, fast-forwarded into the target, and cleaned up.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::project_settings::Delivery;
use orq_orchestrator::{accept_run_delivering, merge_accepted_run, merge_after_accept_pending, summarize_run, Event, RunPhase, RunRecord, RunStore};

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

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success()
}

struct World {
    store: RunStore,
    repo: PathBuf,
    worktree: PathBuf,
}

/// A repository with `main` checked out and a linked worktree on `fix--COM-02` holding one commit of its own
/// (`mine.txt`), recorded as a run delivered `delivery` whose review escalated.
fn world(tag: &str, delivery: Delivery) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-accept-merge-local-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let worktree = root.join("worktree");
    git(&repo, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);

    let store = RunStore::new(&root.join("state"));
    let record = RunRecord {
        delivery,
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: base,
        branch: "fix--COM-02".to_string(),
        worktree: worktree.clone(),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.start_run(&record).unwrap();
    store.append_event(&event("run.completed", serde_json::json!({ "ending": "needs_review", "reason": "the review escalated" }))).unwrap();
    World { store, repo, worktree }
}

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "repo-1".into(),
        feature_id: "COM-02".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-19T10:00:00Z".into(),
        payload,
    }
}

fn payloads(store: &RunStore, kind: &str) -> Vec<serde_json::Value> {
    store.events("run-1").unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect()
}

fn phase(store: &RunStore) -> RunPhase {
    summarize_run(&store.events("run-1").unwrap()).phase
}

#[test]
fn an_accept_whose_branch_starts_at_the_target_tip_merges_and_cleans_up() {
    let w = world("ff", Delivery::MergeLocal);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);
    let completed_asked = RefCell::new(Vec::new());
    let complete = |project: &str, features: &[String], _: &str| -> Result<Vec<String>, String> {
        completed_asked.borrow_mut().push((project.to_string(), features.to_vec()));
        Ok(features.to_vec())
    };

    let said = accept_run_delivering(&w.store, "run-1", "web", &|_: &Path| Ok(()), &complete).unwrap();

    assert!(said.contains("merged into main"), "{said}");
    assert_eq!(git(&w.repo, &["rev-parse", "main"]), head, "main is fast-forwarded to the tested head");
    assert!(!w.worktree.exists(), "the worktree is removed");
    assert!(!git_ok(&w.repo, &["rev-parse", "--verify", "--quiet", "refs/heads/fix--COM-02"]), "the branch is deleted");
    assert_eq!(completed_asked.into_inner(), vec![("repo-1".to_string(), vec!["COM-02".to_string()])]);
    assert_eq!(payloads(&w.store, "ledger.completed_by_run"), vec![serde_json::json!({ "features": ["COM-02"] })]);
    assert_eq!(payloads(&w.store, "merge.completed").len(), 1);
    let completed = payloads(&w.store, "run.completed").pop().unwrap();
    assert_eq!(completed["ending"], "merged", "{completed}");
    assert_eq!(completed["note"], "accepted: tests passed, merged into main", "{completed}");
    assert_eq!(completed["by"], "web", "{completed}");
    assert_eq!(phase(&w.store), RunPhase::Completed);
}

fn no_ledger(_: &str, _: &[String], _: &str) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// A commit on `main` in the checkout, writing `file`.
fn move_target(w: &World, file: &str, contents: &str) -> String {
    fs::write(w.repo.join(file), contents).unwrap();
    git(&w.repo, &["add", "."]);
    git(&w.repo, &["commit", "-q", "-m", "moved"]);
    git(&w.repo, &["rev-parse", "HEAD"])
}

#[test]
fn a_target_that_moved_without_conflict_is_rebased_onto_tested_again_and_merged() {
    let w = world("moved", Delivery::MergeLocal);
    let before = git(&w.worktree, &["rev-parse", "HEAD"]);
    let moved = move_target(&w, "theirs.txt", "theirs\n");
    let tested = RefCell::new(Vec::new());
    let tests = |dir: &Path| {
        tested.borrow_mut().push(git(dir, &["rev-parse", "HEAD"]));
        Ok(())
    };

    accept_run_delivering(&w.store, "run-1", "web", &tests, &no_ledger).unwrap();

    let tested = tested.into_inner();
    assert_eq!(tested.len(), 2, "the tests run on the head accepted and again on the rebased one: {tested:?}");
    assert_eq!(tested[0], before);
    assert_ne!(tested[1], before);
    let main = git(&w.repo, &["rev-parse", "main"]);
    assert_eq!(main, tested[1], "main is fast-forwarded to the head tested after the rebase");
    assert!(git_ok(&w.repo, &["merge-base", "--is-ancestor", &moved, &main]));
    assert_eq!(payloads(&w.store, "run.completed").pop().unwrap()["ending"], "merged");
}

#[test]
fn a_rebase_conflict_fails_the_accept_naming_the_files_and_leaves_the_target_alone() {
    let w = world("conflict", Delivery::MergeLocal);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);
    let moved = move_target(&w, "mine.txt", "theirs\n");

    let err = accept_run_delivering(&w.store, "run-1", "web", &|_: &Path| Ok(()), &no_ledger).unwrap_err();

    assert!(err.contains("mine.txt"), "{err}");
    assert_eq!(git(&w.repo, &["rev-parse", "main"]), moved, "main is untouched");
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), head, "the rebase is undone");
    assert!(w.worktree.exists(), "the worktree is kept");
    let failed = payloads(&w.store, "run.accept_failed").pop().unwrap();
    assert!(failed["reason"].as_str().unwrap().contains("mine.txt"), "{failed}");
    assert_eq!(phase(&w.store), RunPhase::NeedsReview, "the run is back where it was");
}

#[test]
fn a_dirty_checkout_of_the_target_ends_the_accept_blocked() {
    let w = world("dirty", Delivery::MergeLocal);
    // The person's own edit in main's checkout, to a file the run also adds.
    fs::write(w.repo.join("mine.txt"), "my own\n").unwrap();
    git(&w.repo, &["add", "mine.txt"]);

    let err = accept_run_delivering(&w.store, "run-1", "web", &|_: &Path| Ok(()), &no_ledger).unwrap_err();

    assert!(err.contains("uncommitted changes"), "{err}");
    assert_eq!(phase(&w.store), RunPhase::Blocked);
    assert!(orq_orchestrator::waiting_on_dirty_checkout(&w.store.events("run-1").unwrap()), "the run waits on the set-aside merge");
    assert!(w.worktree.exists());
}

#[test]
fn a_merge_local_run_accepted_before_it_merged_on_accept_can_be_merged() {
    let w = world("accepted-before", Delivery::MergeLocal);
    // How an accept ended a merge-local run before accepting merged it.
    let accepted = git(&w.worktree, &["rev-parse", "HEAD"]);
    w.store.append_event(&event("run.accept_started", serde_json::json!({ "by": "web", "head": accepted }))).unwrap();
    w.store.append_event(&event("run.completed", serde_json::json!({ "ending": "review_ready", "note": "accepted: tests passed", "by": "web" }))).unwrap();
    let events = w.store.events("run-1").unwrap();
    let record = w.store.load("run-1").unwrap();
    assert!(merge_after_accept_pending(&record, &events));
    let moved = move_target(&w, "theirs.txt", "theirs\n");
    let tested = RefCell::new(0);
    let tests = |_: &Path| {
        *tested.borrow_mut() += 1;
        Ok(())
    };
    let complete = |_: &str, features: &[String], _: &str| Ok(features.to_vec());

    let said = merge_accepted_run(&w.store, "run-1", "web", &tests, &complete).unwrap();

    assert!(said.contains("merged into main"), "{said}");
    assert_eq!(*tested.borrow(), 1, "the tests run again only because the rebase changed the head");
    let main = git(&w.repo, &["rev-parse", "main"]);
    assert!(git_ok(&w.repo, &["merge-base", "--is-ancestor", &moved, &main]));
    assert!(git_ok(&w.repo, &["cat-file", "-e", &format!("{main}:mine.txt")]), "main holds the run's work");
    assert!(!w.worktree.exists());
    assert_eq!(payloads(&w.store, "ledger.completed_by_run"), vec![serde_json::json!({ "features": ["COM-02"] })]);
    assert_eq!(payloads(&w.store, "run.completed").pop().unwrap()["ending"], "merged");
    assert!(!merge_after_accept_pending(&record, &w.store.events("run-1").unwrap()), "a merged run has nothing left to merge");
}

#[test]
fn a_commit_made_after_the_accept_is_tested_before_the_accepted_run_is_merged() {
    let w = world("committed-after", Delivery::MergeLocal);
    let accepted = git(&w.worktree, &["rev-parse", "HEAD"]);
    w.store.append_event(&event("run.accept_started", serde_json::json!({ "by": "web", "head": accepted }))).unwrap();
    w.store.append_event(&event("run.completed", serde_json::json!({ "ending": "review_ready", "note": "accepted: tests passed", "by": "web" }))).unwrap();
    fs::write(w.worktree.join("later.txt"), "later\n").unwrap();
    git(&w.worktree, &["add", "."]);
    git(&w.worktree, &["commit", "-q", "-m", "after the accept"]);
    let main = git(&w.repo, &["rev-parse", "main"]);

    let err = merge_accepted_run(&w.store, "run-1", "web", &|_: &Path| Err("later.txt breaks a test".to_string()), &no_ledger).unwrap_err();

    assert!(err.contains("later.txt breaks a test"), "the head the accept never tested is tested: {err}");
    assert_eq!(git(&w.repo, &["rev-parse", "main"]), main, "main is untouched");
    assert!(w.worktree.exists());
    assert!(payloads(&w.store, "merge.completed").is_empty());
}

#[test]
fn only_an_accepted_merge_local_run_can_be_merged_after_its_accept() {
    let w = world("not-accepted", Delivery::MergeLocal);
    let err = merge_accepted_run(&w.store, "run-1", "web", &|_: &Path| Ok(()), &no_ledger).unwrap_err();
    assert!(err.contains("accepted"), "{err}");
    assert_eq!(git(&w.repo, &["rev-parse", "main"]), git(&w.repo, &["rev-parse", "HEAD"]));
    assert!(w.worktree.exists());
    assert_eq!(phase(&w.store), RunPhase::NeedsReview);
}

#[test]
fn a_review_delivered_run_still_ends_review_ready_without_merging() {
    let w = world("review", Delivery::Review);
    let main = git(&w.repo, &["rev-parse", "main"]);

    accept_run_delivering(&w.store, "run-1", "web", &|_: &Path| Ok(()), &no_ledger).unwrap();

    assert_eq!(git(&w.repo, &["rev-parse", "main"]), main);
    assert!(w.worktree.exists());
    assert_eq!(payloads(&w.store, "run.completed").pop().unwrap()["ending"], "review_ready");
    assert!(payloads(&w.store, "merge.completed").is_empty());
}
