//! A discard learns what its branch holds before it removes the worktree, so git failing to tell
//! once the worktree is gone cannot leave a run whose worktree is deleted but whose record and
//! branch stay. Its own test binary: it puts a git that refuses to inspect commits on `PATH`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{discard_run_before_remove, Event, RunRecord, RunStore};
use orq_test_support::GitFixture;

/// Puts `PATH` back as it was when dropped, also when the test panics.
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

/// From now until the guard drops, `git rev-parse` and `git rev-list` fail; other commands run the real git.
fn git_that_cannot_inspect_commits(root: &Path) -> RestorePath {
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("a git on PATH");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!("#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in rev-parse|rev-list) exit 128;; esac; done\nexec '{}' \"$@\"\n", real.display())).unwrap();
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
fn a_branch_whose_commits_cannot_be_inspected_once_its_worktree_is_gone_was_inspected_before() {
    let root = orq_test_support::process_temp_dir().join(format!("orq-discard-inspect-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    fs::write(worktree.join("ahead.txt"), "committed work\n").unwrap();
    git_in(&worktree, &["add", "."]);
    git_in(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", "ahead"]);
    let last = git_in(&worktree, &["rev-parse", "HEAD"]);
    let store = RunStore::new(&root.join("orq"));
    store
        .start_run(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: git_in(repo.root(), &["symbolic-ref", "--short", "HEAD"]),
            base_sha: "abc".into(),
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
            kind: "run.failed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();

    let mut guard = None;
    let outcome = discard_run_before_remove(&store, "run-1", repo.root(), &mut |_, _| {}, &mut || {
        guard = Some(git_that_cannot_inspect_commits(&root));
    });
    drop(guard);

    let deleted = outcome.expect("the discard finishes").deleted_branch.expect("the branch was deleted");
    assert_eq!((deleted.commits_ahead, deleted.last_commit.as_str()), (Some(1), last.as_str()));
    assert!(!worktree.exists() && store.load("run-1").is_err());
}
