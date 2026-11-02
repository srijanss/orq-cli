use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{cleanup_worktree, CiOutcome, CleanupOutcome, RunRecord, RunStore};

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
}

/// A repository on `main` with a linked worktree on `fix--COM-02` holding one commit of its own.
fn world(tag: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-cleanup-{}-{tag}", std::process::id()));
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
        })
        .unwrap();
    World { store, repo, worktree }
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn a_review_ready_run_with_ci_disabled_has_its_worktree_removed_and_its_branch_kept() {
    let w = world("disabled");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
    let listed = git(&w.repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains(w.worktree.to_str().unwrap()), "no longer registered: {listed}");
    git(&w.repo, &["rev-parse", "--verify", "refs/heads/fix--COM-02"]);
    assert_eq!(kinds(&w), ["worktree.removed"]);
}

#[test]
fn a_run_whose_ci_passed_has_its_worktree_removed() {
    let w = world("ci-green");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists());
}

#[test]
fn a_run_whose_ci_did_not_pass_keeps_its_worktree() {
    for (tag, ci) in [
        ("ci-red", CiOutcome::Failed { failing: vec!["build".to_string()] }),
        ("ci-timeout", CiOutcome::TimedOut),
        ("ci-error", CiOutcome::Errored { reason: "provider down".to_string() }),
    ] {
        let w = world(tag);

        let outcome = cleanup_worktree(&w.store, "run-1", true, &ci).unwrap();

        assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{tag}: {outcome:?}");
        assert!(w.worktree.join("mine.txt").exists(), "{tag}: the worktree is untouched");
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}

#[test]
fn a_run_that_did_not_reach_review_ready_keeps_its_worktree() {
    let w = world("not-ready");

    let outcome = cleanup_worktree(&w.store, "run-1", false, &CiOutcome::Skipped).unwrap();

    assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{outcome:?}");
    assert!(w.worktree.join("mine.txt").exists());
}

#[test]
fn a_worktree_holding_an_ignored_file_is_kept_with_it_intact() {
    let w = world("ignored");
    fs::write(w.worktree.join(".gitignore"), "local.env\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore local config"]);
    fs::write(w.worktree.join("local.env"), "SECRET=1\n").unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{outcome:?}");
    assert_eq!(fs::read_to_string(w.worktree.join("local.env")).unwrap(), "SECRET=1\n");
    assert_eq!(kinds(&w), ["worktree.kept"]);
}

#[test]
fn a_locked_worktree_is_kept_registered_with_its_branch() {
    let w = world("locked");
    git(&w.repo, &["worktree", "lock", "--reason", "in use", w.worktree.to_str().unwrap()]);

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else { panic!("expected Kept, got {outcome:?}") };
    assert!(reason.contains("would not remove"), "{reason}");
    assert!(w.worktree.join("mine.txt").exists());
    let listed = git(&w.repo, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(w.worktree.to_str().unwrap()), "still registered: {listed}");
    git(&w.repo, &["rev-parse", "--verify", "refs/heads/fix--COM-02"]);
    assert_eq!(kinds(&w), ["worktree.kept"]);
}

#[test]
fn a_worktree_directory_that_is_already_gone_is_kept_with_a_recorded_reason() {
    let w = world("missing");
    fs::remove_dir_all(&w.worktree).unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed);

    assert!(matches!(outcome, Ok(CleanupOutcome::Kept { .. })), "{outcome:?}");
    assert_eq!(kinds(&w), ["worktree.kept"]);
    git(&w.repo, &["rev-parse", "--verify", "refs/heads/fix--COM-02"]);
}

#[test]
fn a_worktree_with_uncommitted_changes_is_kept_with_them_intact() {
    for (tag, edit) in [("modified", "mine.txt"), ("untracked", "scratch.txt")] {
        let w = world(tag);
        fs::write(w.worktree.join(edit), "unsaved work\n").unwrap();

        let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

        assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{tag}: {outcome:?}");
        assert_eq!(fs::read_to_string(w.worktree.join(edit)).unwrap(), "unsaved work\n", "{tag}");
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}
