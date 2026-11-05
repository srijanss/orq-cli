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

fn head_sha(repo: &Path) -> String {
    let out = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn failed_run_without_worktree(tag: &str, repo: &GitFixture, base_sha: &str) -> RunStore {
    let (_, store) = store(tag);
    let gone = orq_test_support::process_temp_dir().join(format!("orq-discard-gone-{}-{tag}", std::process::id()));
    let mut rec = record(&gone, "orq-run-1");
    rec.base_sha = base_sha.to_string();
    let _ = repo;
    store.start_run(&rec).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();
    store
}

#[test]
fn a_run_whose_worktree_is_already_gone_is_forgotten_without_deleting_any_branch() {
    // Without its worktree nothing ties the run to a repository, so no branch is touched there.
    for (tag, same_repo) in [("same", true), ("other-clone", false)] {
        let repo = GitFixture::new();
        let base = head_sha(repo.root());
        let wt = repo.add_linked_worktree("orq-run-1");
        Command::new("git").arg("-C").arg(repo.root()).args(["worktree", "remove", "--force"]).arg(&wt).status().unwrap();
        let other = GitFixture::new();
        other.add_linked_worktree("orq-run-1");
        let supplied = if same_repo { repo.root() } else { other.root() };
        let store = failed_run_without_worktree(tag, &repo, &base);
        discard_run(&store, "run-1", supplied).unwrap();
        assert!(store.load("run-1").is_err(), "{tag}");
        assert_eq!(store.active_run("p", "F").unwrap(), None, "{tag}");
        assert!(branch_exists(repo.root(), "orq-run-1") && branch_exists(other.root(), "orq-run-1"), "{tag}");
    }
}

#[test]
fn a_group_whose_first_member_lock_is_already_gone_still_releases_the_others() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (root, store) = store("lock-gone");
    let members = vec!["F".to_string(), "G".to_string()];
    store.start_group_run(&record(&worktree, "orq-run-1"), &members).unwrap();
    store.append_event(&event("run.grouped", serde_json::json!({ "features": members }))).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();
    std::fs::remove_file(root.join("locks").join("p").join("F.lock")).unwrap();

    discard_run(&store, "run-1", repo.root()).unwrap();

    assert_eq!(store.active_run("p", "G").unwrap(), None);
    assert!(store.load("run-1").is_err());
}

#[test]
fn a_branch_also_checked_out_in_another_worktree_is_never_deleted() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let sibling = worktree.with_file_name("orq-discard-sibling-checkout");
    let _ = std::fs::remove_dir_all(&sibling);
    let added = Command::new("git").arg("-C").arg(repo.root()).args(["worktree", "add", "--force"]).arg(&sibling).arg("orq-run-1").status().unwrap();
    assert!(added.success());
    let (_, store) = store("shared");
    store.start_run(&record(&worktree, "orq-run-1")).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();

    assert!(discard_run(&store, "run-1", repo.root()).is_err());

    assert!(worktree.exists() && sibling.exists() && branch_exists(repo.root(), "orq-run-1"));
    assert!(store.load("run-1").is_ok());
}

#[test]
fn a_lock_that_cannot_be_released_keeps_the_record_so_the_discard_can_be_retried() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (root, store) = store("bad-lock");
    let members = vec!["F".to_string(), "G".to_string()];
    store.start_group_run(&record(&worktree, "orq-run-1"), &members).unwrap();
    store.append_event(&event("run.grouped", serde_json::json!({ "features": members }))).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();
    let lock = root.join("locks").join("p").join("G.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();

    assert!(discard_run(&store, "run-1", repo.root()).is_err());
    assert!(store.load("run-1").is_ok(), "the record survives for a retry");

    std::fs::remove_dir(&lock).unwrap();
    discard_run(&store, "run-1", repo.root()).unwrap();
    assert!(store.load("run-1").is_err());
}

#[test]
fn a_same_branch_sibling_whose_directory_is_gone_still_blocks_the_discard() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let sibling = worktree.with_file_name("orq-discard-stale-sibling");
    let _ = std::fs::remove_dir_all(&sibling);
    let added = Command::new("git").arg("-C").arg(repo.root()).args(["worktree", "add", "--force"]).arg(&sibling).arg("orq-run-1").status().unwrap();
    assert!(added.success());
    std::fs::remove_dir_all(&sibling).unwrap();
    let (_, store) = store("stale-sibling");
    store.start_run(&record(&worktree, "orq-run-1")).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();

    assert!(discard_run(&store, "run-1", repo.root()).is_err());

    assert!(worktree.exists() && branch_exists(repo.root(), "orq-run-1"));
    assert!(store.load("run-1").is_ok());
    assert_eq!(store.active_run("p", "F").unwrap().as_deref(), Some("run-1"));
}
