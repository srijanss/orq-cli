use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{discard_run, Event, RunRecord, RunStore};
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

fn record(worktree: &Path, branch: &str) -> RunRecord {
    RunRecord {
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "abc".into(),
        branch: branch.into(),
        worktree: worktree.to_path_buf(),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn store(tag: &str) -> (PathBuf, RunStore) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-discard-run-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    (root, store)
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_failed_run_is_discarded_with_its_worktree_branch_logs_record_and_every_group_lock() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (root, store) = store("group");
    let members = vec!["F".to_string(), "G".to_string()];
    store.start_group_run(&record(&worktree, "orq-run-1"), &members).unwrap();
    store.append_event(&event("run.grouped", serde_json::json!({ "features": members }))).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();
    std::fs::create_dir_all(root.join("logs").join("run-1")).unwrap();

    discard_run(&store, "run-1", repo.root()).unwrap();

    assert!(store.load("run-1").is_err());
    assert!(!worktree.exists() && !branch_exists(repo.root(), "orq-run-1"));
    assert!(!root.join("logs").join("run-1").exists());
    assert_eq!(store.active_run("p", "F").unwrap(), None);
    assert_eq!(store.active_run("p", "G").unwrap(), None);
}

#[test]
fn a_run_that_is_not_failed_or_has_an_unsafe_worktree_or_branch_is_left_untouched() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let other = repo.add_linked_worktree("other");
    let cases: [(&str, Option<&str>, &Path, &str); 4] = [
        ("running", None, &worktree, "orq-run-1"),
        ("main-worktree", Some("run.failed"), repo.root(), "orq-run-1"),
        ("wrong-branch", Some("run.failed"), &other, "orq-run-1"),
        ("target-branch", Some("run.failed"), &worktree, "main"),
    ];
    for (tag, ending, wt, branch) in cases {
        let (_, store) = store(tag);
        store.start_run(&record(wt, branch)).unwrap();
        if let Some(kind) = ending {
            store.append_event(&event(kind, serde_json::json!({}))).unwrap();
        }
        assert!(discard_run(&store, "run-1", repo.root()).is_err(), "{tag}");
        assert!(store.load("run-1").is_ok(), "{tag}");
        assert_eq!(store.active_run("p", "F").unwrap().as_deref(), Some("run-1"), "{tag}");
    }
    assert!(worktree.exists() && other.exists() && repo.root().exists());
    assert!(branch_exists(repo.root(), "orq-run-1") && branch_exists(repo.root(), "other"));
}
