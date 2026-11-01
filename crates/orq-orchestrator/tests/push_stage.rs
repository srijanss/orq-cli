use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{push_run_branch, PushOutcome, RunRecord, RunStore};

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
    root: PathBuf,
    remote: PathBuf,
    worktree: PathBuf,
    log: PathBuf,
}

/// A bare remote with `main`, and a worktree on `branch` with one commit of its own.
fn world(tag: &str, branch: &str, target: &str, remote_override: Option<&str>) -> World {
    let root = std::env::temp_dir().join(format!("orq-push-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    let worktree = root.join("worktree");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), worktree.to_str().unwrap()]);
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    git(&worktree, &["push", "-q", "origin", "HEAD:main"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["checkout", "-q", "-B", branch]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);

    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: remote_override.map(str::to_string).unwrap_or_else(|| remote.to_str().unwrap().to_string()),
            provider: "github".to_string(),
            target_branch: target.to_string(),
            base_sha: base,
            branch: branch.to_string(),
            worktree: worktree.clone(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let log = root.join("push.log");
    World { store, root, remote, worktree, log }
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn the_run_branch_is_pushed_and_its_head_sha_recorded() {
    let w = world("push", "fix--COM-02", "main", None);
    let head = git(&w.worktree, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert_eq!(outcome, PushOutcome::Pushed { head_sha: head.clone() });
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), head);
    assert_eq!(kinds(&w), ["stage.started", "stage.completed"]);
    let done = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(done.stage.as_deref(), Some("push"));
    assert_eq!(done.payload["pushed_sha"], head.as_str());
}

#[test]
fn the_target_branch_on_the_remote_is_never_touched() {
    let w = world("target", "fix--COM-02", "main", None);
    let before = git(&w.remote, &["rev-parse", "refs/heads/main"]);

    push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/main"]), before);
}

#[test]
fn a_run_branch_named_like_the_target_is_refused_without_pushing() {
    let w = world("same", "main", "main", None);
    let before = git(&w.remote, &["rev-parse", "refs/heads/main"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/main"]), before);
    assert_eq!(kinds(&w), ["stage.failed"]);
}

#[test]
fn a_push_that_git_rejects_fails_the_stage_and_logs_gits_output() {
    let w = world("reject", "fix--COM-02", "main", Some("/nonexistent/remote.git"));

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(kinds(&w), ["stage.started", "stage.failed"]);
    let logged = fs::read_to_string(&w.log).unwrap();
    assert!(logged.contains("nonexistent"), "git's own message is in the log: {logged}");
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert!(failed.payload["log"].as_str().unwrap().ends_with("push.log"));
}

#[test]
fn a_rejected_push_never_overwrites_a_diverged_remote_branch() {
    let w = world("diverged", "fix--COM-02", "main", None);
    // Someone else already pushed a different commit to the same branch name.
    let other = w.root.join("other");
    git(&w.root, &["clone", "-q", w.remote.to_str().unwrap(), other.to_str().unwrap()]);
    git(&other, &["checkout", "-q", "-b", "fix--COM-02"]);
    fs::write(other.join("theirs.txt"), "theirs\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q", "origin", "fix--COM-02"]);
    let theirs = git(&other, &["rev-parse", "HEAD"]);

    let outcome = push_run_branch(&w.store, "run-1", &w.log).unwrap();

    assert!(matches!(outcome, PushOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(git(&w.remote, &["rev-parse", "refs/heads/fix--COM-02"]), theirs);
}

#[test]
fn an_unknown_run_is_an_error() {
    let w = world("unknown", "fix--COM-02", "main", None);

    assert!(push_run_branch(&w.store, "ghost", &w.log).is_err());
}
