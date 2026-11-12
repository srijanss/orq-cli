use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{RunRecord, RunStore, StoreError};

fn store(tag: &str) -> RunStore {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-group-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    RunStore::new(&dir)
}

fn record(run_id: &str, feature: &str) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: feature.to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: format!("fix--{feature}"),
        worktree: PathBuf::from(format!(".claude/worktrees/fix--{feature}")),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn members(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_group_run_holds_the_lock_of_every_member_until_it_finishes() {
    let store = store("holds");
    let group = members(&["a", "b", "c"]);
    store
        .start_group_run(&record("run-1", "a"), &group)
        .unwrap();
    for id in ["a", "b", "c"] {
        assert_eq!(
            store.active_run("repo-1", id).unwrap().as_deref(),
            Some("run-1"),
            "{id}"
        );
    }
    // A run of a non-first member is refused while the group runs.
    assert!(
        matches!(store.start_run(&record("run-2", "b")), Err(StoreError::FeatureBusy { active_run_id }) if active_run_id == "run-1")
    );
    store.finish_group_run("repo-1", &group, "run-1").unwrap();
    for id in ["a", "b", "c"] {
        assert_eq!(store.active_run("repo-1", id).unwrap(), None, "{id}");
    }
}

#[test]
fn a_group_with_a_busy_member_starts_nothing_and_leaves_no_lock_behind() {
    let store = store("busy");
    store.start_run(&record("run-1", "c")).unwrap();
    let group = members(&["a", "b", "c"]);
    let err = store
        .start_group_run(&record("run-2", "a"), &group)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::FeatureBusy { ref active_run_id } if active_run_id == "run-1"),
        "{err:?}"
    );
    assert_eq!(store.active_run("repo-1", "a").unwrap(), None);
    assert_eq!(store.active_run("repo-1", "b").unwrap(), None);
    assert!(store.load("run-2").is_err(), "no run was recorded");
}

#[test]
fn finishing_a_group_releases_every_member_it_still_owns_and_reports_the_rest() {
    let store = store("finish");
    let group = members(&["a", "b"]);
    store
        .start_group_run(&record("run-1", "a"), &group)
        .unwrap();
    // Someone else's lock on b must not be taken, and a is still released.
    store.finish_run("repo-1", "b", "run-1").unwrap();
    store.start_run(&record("run-2", "b")).unwrap();
    assert!(store.finish_group_run("repo-1", &group, "run-1").is_err());
    assert_eq!(store.active_run("repo-1", "a").unwrap(), None);
    assert_eq!(
        store.active_run("repo-1", "b").unwrap().as_deref(),
        Some("run-2")
    );
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
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

/// A stopped (failed) run of feature `a`, in a real worktree, that ran as `group` when one is given.
fn stopped_run(tag: &str, group: Option<&[&str]>) -> (RunStore, PathBuf) {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-group-resume-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let worktree = repo.join(".claude/worktrees/fix--a");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fix--a",
            worktree.to_str().unwrap(),
        ],
    );
    let store = RunStore::new(&root.join("state"));
    let mut rec = record("run-1", "a");
    rec.base_sha = git(&repo, &["rev-parse", "main"]);
    rec.worktree = worktree;
    store.create(&rec).unwrap();
    if let Some(group) = group {
        orq_orchestrator::record_group(&store, &rec, &members(group)).unwrap();
    }
    orq_orchestrator::record_run_completed(&store, &rec, "failed", Some("the gate failed"))
        .unwrap();
    (store, repo)
}

#[test]
fn a_stopped_group_run_resumes_only_as_the_same_ordered_group() {
    let (store, repo) = stopped_run("same", Some(&["a", "b"]));
    let resume =
        |ids: &[&str]| orq_orchestrator::plan_group_resume(&store, &repo, "repo-1", &members(ids));
    assert_eq!(resume(&["a", "b"]).unwrap().from.run_id, "run-1");
    for other in [&["a"][..], &["a", "c"], &["a", "b", "c"], &["a", "c", "b"]] {
        let err = resume(other).unwrap_err();
        assert!(
            err.contains("run-1") && err.contains("a, b"),
            "{other:?}: {err}"
        );
    }
    // A different first feature has no run of its own to resume.
    assert!(resume(&["b", "a"]).is_err());
}

#[test]
fn a_run_of_one_feature_resumes_as_that_feature_and_not_as_a_group() {
    let (store, repo) = stopped_run("single", None);
    assert!(orq_orchestrator::plan_group_resume(&store, &repo, "repo-1", &members(&["a"])).is_ok());
    assert!(orq_orchestrator::plan_resume(&store, &repo, "repo-1", "a").is_ok());
    let err = orq_orchestrator::plan_group_resume(&store, &repo, "repo-1", &members(&["a", "b"]))
        .unwrap_err();
    assert!(err.contains("run-1"), "{err}");
}

fn no_locks(store: &RunStore, ids: &[&str]) {
    for id in ids {
        assert_eq!(
            store.active_run("repo-1", id).unwrap(),
            None,
            "lock of {id}"
        );
    }
}

#[test]
fn failed_start_of_a_group_without_its_own_feature_or_without_members_locks_nothing() {
    let store = store("members");
    for group in [
        members(&[]),
        members(&["b", "c"]),
        members(&["a", "b", "a"]),
    ] {
        assert!(
            store
                .start_group_run(&record("run-1", "a"), &group)
                .is_err(),
            "{group:?}"
        );
        no_locks(&store, &["a", "b", "c"]);
        assert!(
            store.run_ids().unwrap().is_empty(),
            "{group:?} recorded a run"
        );
    }
}

#[test]
fn failed_start_on_a_later_invalid_member_releases_the_earlier_locks() {
    let store = store("invalid");
    let err = store.start_group_run(&record("run-1", "a"), &members(&["a", "b", "../c"]));
    assert!(matches!(err, Err(StoreError::InvalidRunId(_))), "{err:?}");
    no_locks(&store, &["a", "b"]);
    assert!(store.run_ids().unwrap().is_empty());
}

#[test]
fn failed_start_when_the_run_cannot_be_created_keeps_the_existing_run_and_releases_every_lock() {
    let store = store("create");
    store.start_run(&record("run-1", "z")).unwrap();
    store.finish_run("repo-1", "z", "run-1").unwrap();
    // run-1 already exists, so creating it again fails once every lock is held.
    let err = store.start_group_run(&record("run-1", "a"), &members(&["a", "b"]));
    assert!(matches!(err, Err(StoreError::AlreadyExists(_))), "{err:?}");
    no_locks(&store, &["a", "b"]);
    assert_eq!(
        store.load("run-1").unwrap().feature_id,
        "z",
        "the existing run is untouched"
    );
    // A single-feature start fails the same way and keeps an unrelated lock.
    store.start_run(&record("run-2", "other")).unwrap();
    let err = store.start_run(&record("run-1", "a"));
    assert!(matches!(err, Err(StoreError::AlreadyExists(_))), "{err:?}");
    no_locks(&store, &["a"]);
    assert_eq!(
        store.active_run("repo-1", "other").unwrap().as_deref(),
        Some("run-2")
    );
}
