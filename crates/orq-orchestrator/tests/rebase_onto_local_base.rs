//! A merge-local run is rebased before its gate onto the local base branch, which other runs
//! merge into, not onto the remote's copy of it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::project_settings::Delivery;
use orq_orchestrator::{rebase_onto_target, RebaseOutcome, RunRecord, RunStore};

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
    /// The project's checkout, on `main`: committing here moves the local base branch.
    project: PathBuf,
    /// The run's worktree of the project, on `fix--COM-02` with one commit of its own.
    worktree: PathBuf,
    /// Another clone of the remote, to move the remote's `main`.
    other: PathBuf,
}

fn world(tag: &str, delivery: Delivery, remote_override: Option<&str>) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-rebase-local-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    let project = root.join("project");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), project.to_str().unwrap()]);
    for (key, value) in [("user.name", "t"), ("user.email", "t@example.com"), ("commit.gpgsign", "false")] {
        git(&project, &["config", key, value]);
    }
    fs::write(project.join("a.txt"), "a\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "base"]);
    git(&project, &["push", "-q", "origin", "HEAD:main"]);
    let base = git(&project, &["rev-parse", "HEAD"]);
    let worktree = root.join("worktree");
    git(&project, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);
    let other = root.join("other");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);

    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery,
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: remote_override.map(str::to_string).unwrap_or_else(|| remote.to_str().unwrap().to_string()),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base,
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, project, worktree, other }
}

/// Commit `file` on the local `main`, as another run's local merge does.
fn advance_local(w: &World, file: &str, content: &str) -> String {
    fs::write(w.project.join(file), content).unwrap();
    git(&w.project, &["add", "."]);
    git(&w.project, &["commit", "-q", "-m", &format!("local {file}")]);
    git(&w.project, &["rev-parse", "HEAD"])
}

/// Commit `file` in the other clone and push it to the remote's `main`.
fn advance_remote(w: &World, file: &str, content: &str) -> String {
    fs::write(w.other.join(file), content).unwrap();
    git(&w.other, &["add", "."]);
    git(&w.other, &["commit", "-q", "-m", &format!("remote {file}")]);
    git(&w.other, &["push", "-q", "origin", "HEAD:main"]);
    git(&w.other, &["rev-parse", "HEAD"])
}

#[test]
fn a_merge_local_run_is_rebased_onto_the_local_base_branch_ahead_of_the_remote() {
    let w = world("ahead", Delivery::MergeLocal, None);
    let local = advance_local(&w, "local.txt", "local\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: local.clone() });
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD~1"]), local, "the run's commit sits on local main");
    assert!(w.worktree.join("local.txt").is_file());
    let events = w.store.events("run-1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "rebase.completed");
    assert_eq!(events[0].payload["target_sha"], local.as_str());
    assert_eq!(events[0].payload["source"], "local");
}

#[test]
fn a_merge_local_run_is_rebased_without_reaching_the_remote() {
    let w = world("noremote", Delivery::MergeLocal, Some("/nonexistent/orq-remote.git"));
    let local = advance_local(&w, "local.txt", "local\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: local });
}

#[test]
fn a_conflict_with_work_merged_into_the_local_base_is_found_before_the_gate() {
    let w = world("conflict", Delivery::MergeLocal, None);
    fs::write(w.worktree.join("a.txt"), "mine\n").unwrap();
    git(&w.worktree, &["commit", "-q", "-am", "edit a"]);
    let local = advance_local(&w, "a.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert!(matches!(outcome, RebaseOutcome::Conflict { .. }), "{outcome:?}");
    let events = w.store.events("run-1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "rebase.conflict");
    assert_eq!(events[0].payload["files"], serde_json::json!(["a.txt"]));
    assert_eq!(events[0].payload["onto"], local.as_str());
    assert_eq!(events[0].payload["where"], "before_gate");
    assert_eq!(events[0].payload["source"], "local");
}

#[test]
fn a_merge_local_run_takes_none_of_the_remote_commits_when_local_and_remote_diverged() {
    let w = world("diverged", Delivery::MergeLocal, None);
    let local = advance_local(&w, "local.txt", "local\n");
    let remote = advance_remote(&w, "remote.txt", "remote\n");
    git(&w.project, &["fetch", "-q", "origin"]);

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: local.clone() });
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD~1"]), local);
    let contains_remote = Command::new("git")
        .arg("-C")
        .arg(&w.worktree)
        .args(["merge-base", "--is-ancestor", &remote, "HEAD"])
        .status()
        .unwrap()
        .success();
    assert!(!contains_remote, "no remote-only commit is in the run's branch");
    assert!(!w.worktree.join("remote.txt").exists());
}

#[test]
fn a_merge_local_run_whose_local_target_branch_is_missing_stops_naming_the_branch() {
    let w = world("missing", Delivery::MergeLocal, None);
    git(&w.project, &["checkout", "-q", "--detach"]);
    git(&w.project, &["branch", "-q", "-D", "main"]);

    let error = rebase_onto_target(&w.store, "run-1", true).unwrap_err().to_string();

    assert!(error.contains("main"), "{error}");
    assert!(error.contains("local target branch"), "{error}");
    assert!(w.store.events("run-1").unwrap().is_empty());
}

#[test]
fn a_review_run_is_still_rebased_onto_the_remote_target_branch() {
    let w = world("review", Delivery::Review, None);
    advance_local(&w, "local.txt", "local\n");
    let remote = advance_remote(&w, "remote.txt", "remote\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: remote.clone() });
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD~1"]), remote);
    assert!(!w.worktree.join("local.txt").exists());
    let events = w.store.events("run-1").unwrap();
    assert_eq!(events[0].payload["source"], "remote");
}
