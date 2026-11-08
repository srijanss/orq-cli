//! A discard that cannot tell whether a failed run's work is on its target branch stops with
//! nothing removed instead of reopening the run's features. Its own test binary: it puts a git
//! that refuses to list commits on `PATH`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{discard_run_then, Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

/// Puts `PATH` back as it was when dropped, also when the test panics.
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

/// From now until the guard drops, `git rev-list` fails; other commands run the real git.
fn git_that_cannot_list_commits(root: &Path) -> RestorePath {
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("a git on PATH");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!("#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in rev-list) exit 128;; esac; done\nexec '{}' \"$@\"\n", real.display())).unwrap();
    fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    RestorePath(path)
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a_failed_run_whose_merge_check_git_cannot_answer_is_not_discarded_and_its_features_are_not_reopened() {
    let root = orq_test_support::process_temp_dir().join(format!("orq-discard-merge-check-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = GitFixture::new();
    let base = git_in(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    fs::write(worktree.join("ahead.txt"), "committed work\n").unwrap();
    git_in(&worktree, &["add", "."]);
    git_in(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", "ahead"]);
    // The worktree is gone but the branch stays, so the discard has only the merge check to run.
    git_in(repo.root(), &["worktree", "remove", "--force", worktree.to_str().unwrap()]);
    let store = RunStore::new(&root.join("orq"));
    store
        .start_run(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: git_in(repo.root(), &["symbolic-ref", "--short", "HEAD"]),
            base_sha: base,
            branch: "orq-run-1".into(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e-failed".into(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "run.completed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({ "ending": "failed" }),
        })
        .unwrap();

    let mut reopened = Vec::new();
    let guard = git_that_cannot_list_commits(&root);
    let outcome = discard_run_then(&store, "run-1", repo.root(), &mut |_, features| reopened.extend_from_slice(features));
    drop(guard);

    let refused = outcome.expect_err("the discard stops");
    assert!(refused.contains("target branch"), "{refused}");
    assert!(reopened.is_empty(), "features reopened: {reopened:?}");
    assert!(store.load("run-1").is_ok(), "the record stays");
    let _ = fs::remove_dir_all(&root);
}
