use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{accept_run, summarize_run, Event, RunPhase, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn event(kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A failed run whose worktree has `commits` commits on its branch beyond the base.
fn failed_run(tag: &str, commits: usize) -> (GitFixture, PathBuf, RunStore) {
    failed_run_recorded_as(tag, commits, |worktree| worktree.to_path_buf())
}

/// Like `failed_run`, but the record names `recorded(worktree)` as the run's worktree.
fn failed_run_recorded_as(tag: &str, commits: usize, recorded: impl Fn(&Path) -> PathBuf) -> (GitFixture, PathBuf, RunStore) {
    let repo = GitFixture::new();
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    for n in 0..commits {
        std::fs::write(worktree.join(format!("work{n}.txt")), "work").unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "work"]);
    }
    let root = orq_test_support::process_temp_dir().join(format!("orq-accept-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    let record = RunRecord {
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: base,
        branch: "orq-run-1".into(),
        worktree: recorded(&worktree),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.start_run(&record).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({ "reason": "the implement session reported a problem" }))).unwrap();
    (repo, worktree, store)
}

fn phase(store: &RunStore) -> RunPhase {
    summarize_run(&store.events("run-1").unwrap()).phase
}

#[test]
fn a_failed_run_with_committed_work_whose_tests_pass_becomes_review_ready() {
    let (_repo, worktree, store) = failed_run("pass", 1);
    let seen = RefCell::new(None);
    let outcome = accept_run(&store, "run-1", &|dir: &Path| {
        *seen.borrow_mut() = Some(dir.to_path_buf());
        Ok(())
    });
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(seen.into_inner(), Some(worktree));
    assert_eq!(phase(&store), RunPhase::Completed);
}

#[test]
fn tests_that_fail_leave_the_run_failed_and_say_why() {
    let (_repo, _worktree, store) = failed_run("tests-fail", 1);
    let err = accept_run(&store, "run-1", &|_: &Path| Err("2 tests failed".to_string())).unwrap_err();
    assert!(err.contains("2 tests failed"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);
}

#[test]
fn a_run_with_nothing_committed_or_uncommitted_changes_or_no_worktree_is_not_accepted() {
    let ran = RefCell::new(false);
    let never = |_: &Path| {
        *ran.borrow_mut() = true;
        Ok(())
    };

    let (_repo, _worktree, store) = failed_run("no-commits", 0);
    assert!(accept_run(&store, "run-1", &never).is_err());
    assert_eq!(phase(&store), RunPhase::Failed);

    let (_repo2, dirty, store) = failed_run("dirty", 1);
    std::fs::write(dirty.join("loose.txt"), "not committed").unwrap();
    assert!(accept_run(&store, "run-1", &never).is_err());
    assert_eq!(phase(&store), RunPhase::Failed);

    let (_repo3, gone, store) = failed_run("gone", 1);
    git(_repo3.root(), &["worktree", "remove", "--force", gone.to_str().unwrap()]);
    assert!(accept_run(&store, "run-1", &never).is_err());
    assert_eq!(phase(&store), RunPhase::Failed);

    assert!(!*ran.borrow(), "the tests are not run for a run that cannot be accepted");
}

#[test]
fn only_a_failed_run_can_be_accepted() {
    let (_repo, _worktree, store) = failed_run("blocked", 1);
    store.append_event(&event("run.blocked", serde_json::json!({}))).unwrap();
    assert!(accept_run(&store, "run-1", &|_: &Path| Ok(())).is_err());
    assert_eq!(phase(&store), RunPhase::Blocked);
}

#[test]
fn a_directory_that_is_not_the_runs_own_worktree_is_not_accepted() {
    let ran = RefCell::new(false);
    let never = |_: &Path| {
        *ran.borrow_mut() = true;
        Ok(())
    };

    // The worktree was switched to another branch: its work is not the run's work.
    let (_repo, worktree, store) = failed_run("other-branch", 1);
    git(&worktree, &["checkout", "-q", "-b", "someone-elses-branch"]);
    let err = accept_run(&store, "run-1", &never).unwrap_err();
    assert!(err.contains("orq-run-1"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);

    // The recorded path is a subdirectory of the worktree, not its root.
    let (_repo2, _worktree2, store2) = failed_run_recorded_as("subdir", 1, |worktree| {
        let sub = worktree.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        sub
    });
    assert!(accept_run(&store2, "run-1", &never).is_err());
    assert_eq!(phase(&store2), RunPhase::Failed);

    assert!(!*ran.borrow(), "the tests are not run in a directory that is not the run's worktree");
}

#[test]
fn tests_that_switch_the_branch_or_detach_head_at_the_same_commit_do_not_make_the_run_review_ready() {
    let (_repo, _worktree, store) = failed_run("switched", 1);
    let err = accept_run(&store, "run-1", &|dir: &Path| {
        git(dir, &["checkout", "-q", "-b", "someone-elses-branch"]);
        Ok(())
    })
    .unwrap_err();
    assert!(err.contains("orq-run-1"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);

    let (_repo2, _worktree2, store) = failed_run("detached", 1);
    let err = accept_run(&store, "run-1", &|dir: &Path| {
        git(dir, &["checkout", "-q", "--detach"]);
        Ok(())
    })
    .unwrap_err();
    assert!(err.contains("orq-run-1"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);
}

#[test]
fn tests_that_leave_the_worktree_changed_or_move_its_head_do_not_make_the_run_review_ready() {
    let (_repo, _worktree, store) = failed_run("mutated", 1);
    let err = accept_run(&store, "run-1", &|dir: &Path| {
        std::fs::write(dir.join("generated.txt"), "left by the tests").unwrap();
        Ok(())
    })
    .unwrap_err();
    assert!(err.contains("changed the worktree"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);

    let (_repo2, _worktree2, store) = failed_run("moved", 1);
    let err = accept_run(&store, "run-1", &|dir: &Path| {
        std::fs::write(dir.join("more.txt"), "x").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "moved"]);
        Ok(())
    })
    .unwrap_err();
    assert!(err.contains("changed the worktree"), "{err}");
    assert_eq!(phase(&store), RunPhase::Failed);
}
