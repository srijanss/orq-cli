//! A merge-local run delivers its work by fast-forwarding the base branch of its own repository to
//! the run's branch. Nothing is pushed and no merge commit is ever made.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{merge_into_base, Event, MergeOutcome, RunRecord, RunStore};

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

struct World {
    store: RunStore,
    repo: PathBuf,
    worktree: PathBuf,
    base: String,
}

/// A repository on `main` with a linked worktree on `fix--COM-02` holding one commit of its own.
fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-merge-local-{}-{tag}", std::process::id()));
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
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base.clone(),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, repo, worktree, base }
}

#[test]
fn a_checked_out_base_branch_is_fast_forwarded_in_its_own_checkout() {
    let w = world("checked-out");
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert_eq!(merge_into_base(&w.store, "run-1").unwrap(), MergeOutcome::Merged { head_sha: head.clone() });
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), head);
    assert!(w.repo.join("mine.txt").exists(), "the checkout was updated, not just its ref");
    assert_eq!(git(&w.repo, &["status", "--porcelain"]), "", "the checkout is clean afterwards");
    assert_eq!(git(&w.repo, &["rev-list", "--merges", &format!("{}..main", w.base)]), "", "no merge commit");
}

#[test]
fn a_base_branch_no_checkout_has_is_moved_from_where_the_run_started() {
    let w = world("not-checked-out");
    git(&w.repo, &["checkout", "-q", "-b", "elsewhere"]);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert_eq!(merge_into_base(&w.store, "run-1").unwrap(), MergeOutcome::Merged { head_sha: head.clone() });
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), head);
    assert_eq!(git(&w.repo, &["symbolic-ref", "HEAD"]), "refs/heads/elsewhere", "the checkout stays where it was");
    assert_eq!(git(&w.repo, &["rev-parse", "elsewhere"]), w.base, "and so does its branch");
    assert!(!w.repo.join("mine.txt").exists(), "nothing was checked out");
}

#[test]
fn a_checkout_with_uncommitted_changes_is_refused_and_left_alone() {
    let w = world("dirty");
    fs::write(w.repo.join("a.txt"), "edited\n").unwrap();

    match merge_into_base(&w.store, "run-1").unwrap() {
        MergeOutcome::Refused { reason } => assert!(reason.contains("uncommitted changes"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), w.base, "main did not move");
    assert_eq!(fs::read_to_string(w.repo.join("a.txt")).unwrap(), "edited\n", "the edit is left alone");
}

#[test]
fn a_checked_out_base_branch_that_moved_since_the_run_started_is_refused() {
    let w = world("moved");
    fs::write(w.repo.join("theirs.txt"), "theirs\n").unwrap();
    git(&w.repo, &["add", "."]);
    git(&w.repo, &["commit", "-q", "-m", "theirs"]);
    let moved = git(&w.repo, &["rev-parse", "HEAD"]);

    match merge_into_base(&w.store, "run-1").unwrap() {
        MergeOutcome::Refused { reason } => assert!(reason.contains("moved"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), moved, "main stays where it moved to");
}

/// The run was rebased onto a fetched target that is ahead of the local base branch, as when the
/// remote has commits the local `main` has not pulled yet.
fn rebased_onto_a_target_ahead_of_local_main(w: &World) -> String {
    git(&w.worktree, &["reset", "-q", "--hard", &w.base]);
    fs::write(w.worktree.join("upstream.txt"), "upstream\n").unwrap();
    git(&w.worktree, &["add", "."]);
    git(&w.worktree, &["commit", "-q", "-m", "upstream"]);
    let upstream = git(&w.worktree, &["rev-parse", "HEAD"]);
    fs::write(w.worktree.join("mine.txt"), "mine\n").unwrap();
    git(&w.worktree, &["add", "."]);
    git(&w.worktree, &["commit", "-q", "-m", "mine"]);
    w.store
        .append_event(&Event {
            schema_version: 1,
            event_id: "rebase-1".to_string(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            kind: "rebase.completed".to_string(),
            stage: Some("rebase".to_string()),
            attempt: 1,
            occurred_at: "2026-11-13T00:00:00Z".to_string(),
            payload: serde_json::json!({ "target_sha": upstream }),
        })
        .unwrap();
    upstream
}

#[test]
fn a_base_branch_no_checkout_has_and_behind_the_fetched_target_is_fast_forwarded_from_where_it_is() {
    let w = world("behind-fetched");
    git(&w.repo, &["checkout", "-q", "-b", "elsewhere"]);
    rebased_onto_a_target_ahead_of_local_main(&w);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert_eq!(merge_into_base(&w.store, "run-1").unwrap(), MergeOutcome::Merged { head_sha: head.clone() });
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), head);
}

#[test]
fn an_ignored_local_file_the_run_would_overwrite_is_refused_and_kept() {
    let w = world("ignored");
    fs::write(w.repo.join(".git/info/exclude"), "mine.txt\n").unwrap();
    fs::write(w.repo.join("mine.txt"), "my local notes\n").unwrap();

    match merge_into_base(&w.store, "run-1").unwrap() {
        MergeOutcome::Refused { reason } => assert!(reason.contains("mine.txt"), "the reason names the file: {reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(w.repo.join("mine.txt")).unwrap(), "my local notes\n", "the local file is kept");
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), w.base, "main did not move");
}

#[test]
fn a_base_branch_checked_out_at_a_path_with_a_newline_is_fast_forwarded_there() {
    let w = world("odd-path");
    git(&w.repo, &["checkout", "-q", "-b", "elsewhere"]);
    let odd = w.repo.parent().unwrap().join("main\ncheckout");
    git(&w.repo, &["worktree", "add", "-q", odd.to_str().unwrap(), "main"]);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert_eq!(merge_into_base(&w.store, "run-1").unwrap(), MergeOutcome::Merged { head_sha: head.clone() });
    assert_eq!(git(&w.repo, &["rev-parse", "refs/heads/main"]), head);
    assert!(odd.join("mine.txt").exists(), "the checkout at that path was updated");
}
