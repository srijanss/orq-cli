use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{discard_preview, discard_run, discard_run_before_remove, discard_run_then, Event, RunRecord, RunStore, StoreError};
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
        delivery: Default::default(),
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

#[test]
fn a_blocked_run_is_discarded_like_a_failed_one() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (root, store) = store("blocked");
    store.start_run(&record(&worktree, "orq-run-1")).unwrap();
    store.append_event(&event("run.blocked", serde_json::json!({}))).unwrap();
    std::fs::create_dir_all(root.join("logs").join("run-1")).unwrap();

    discard_run(&store, "run-1", repo.root()).unwrap();

    assert!(store.load("run-1").is_err());
    assert!(!worktree.exists() && !branch_exists(repo.root(), "orq-run-1"));
    assert!(!root.join("logs").join("run-1").exists());
    assert_eq!(store.active_run("p", "F").unwrap(), None);
}

#[test]
fn reopen_the_discard_holds_every_orphaned_feature_lock_while_the_features_are_reopened() {
    // The run already released F's lock when it ended; G another run has since taken.
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (_, store) = store("reopen-held");
    let members = vec!["F".to_string(), "G".to_string(), "H".to_string()];
    store.start_group_run(&record(&worktree, "orq-run-1"), &members).unwrap();
    store.append_event(&event("run.grouped", serde_json::json!({ "features": members }))).unwrap();
    store.append_event(&event("run.blocked", serde_json::json!({}))).unwrap();
    store.finish_group_run("p", &["F".to_string(), "G".to_string()], "run-1").unwrap();
    let mut other = record(&worktree, "orq-run-2");
    other.run_id = "run-2".into();
    other.feature_id = "G".into();
    store.start_run(&other).unwrap();

    let mut seen = Vec::new();
    discard_run_then(&store, "run-1", repo.root(), &mut |project_id, features| {
        assert_eq!(project_id, "p");
        for feature in features {
            assert_eq!(store.active_run("p", feature).unwrap().as_deref(), Some("run-1"), "{feature} is held while it is reopened");
        }
        // A run launched now cannot take a feature that is being reopened.
        let mut late = record(&worktree, "orq-run-3");
        late.run_id = "run-3".into();
        assert!(matches!(store.start_run(&late), Err(StoreError::FeatureBusy { .. })));
        seen = features.to_vec();
    })
    .unwrap();

    assert_eq!(seen, vec!["F".to_string(), "H".to_string()], "G belongs to run-2");
    assert_eq!(store.active_run("p", "F").unwrap(), None);
    assert_eq!(store.active_run("p", "H").unwrap(), None);
    assert_eq!(store.active_run("p", "G").unwrap().as_deref(), Some("run-2"));
}

#[test]
fn reopen_a_lock_that_cannot_be_claimed_frees_the_locks_the_discard_took_and_keeps_the_run_s_own() {
    // F was released when the run ended; H the run still holds; the third member's lock cannot be taken.
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (_, store) = store("reopen-claim-fails");
    let members = vec!["F".to_string(), "H".to_string()];
    store.start_group_run(&record(&worktree, "orq-run-1"), &members).unwrap();
    store
        .append_event(&event("run.grouped", serde_json::json!({ "features": ["F", "H", "not a feature id"] })))
        .unwrap();
    store.append_event(&event("run.blocked", serde_json::json!({}))).unwrap();
    store.finish_group_run("p", &["F".to_string()], "run-1").unwrap();

    let mut reopened = false;
    let result = discard_run_then(&store, "run-1", repo.root(), &mut |_, _| reopened = true);

    assert!(result.is_err());
    assert!(!reopened, "nothing is reopened when the locks could not all be claimed");
    assert_eq!(store.active_run("p", "F").unwrap(), None, "the lock the discard took is given back");
    assert_eq!(store.active_run("p", "H").unwrap().as_deref(), Some("run-1"), "the run's own lock stays for a retry");
    assert!(store.load("run-1").is_ok(), "the record stays, so the discard can be retried");
}

fn paused_run(tag: &str, repo: &GitFixture) -> (PathBuf, PathBuf, RunStore) {
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (root, store) = store(tag);
    store.start_run(&record(&worktree, "orq-run-1")).unwrap();
    store.append_event(&event("run.pause_requested", serde_json::json!({}))).unwrap();
    store.append_event(&event("run.paused", serde_json::json!({}))).unwrap();
    std::fs::create_dir_all(root.join("logs").join("run-1")).unwrap();
    (root, worktree, store)
}

#[test]
fn discard_keep_a_paused_run_with_a_clean_worktree_is_discarded_with_its_worktree_and_branch() {
    let repo = GitFixture::new();
    let (root, worktree, store) = paused_run("paused-clean", &repo);

    let outcome = discard_run(&store, "run-1", repo.root()).unwrap();

    assert_eq!(outcome.kept_worktree, None);
    assert_eq!(outcome.features, vec!["F".to_string()]);
    assert!(store.load("run-1").is_err());
    assert!(!worktree.exists() && !branch_exists(repo.root(), "orq-run-1"));
    assert!(!root.join("logs").join("run-1").exists());
    assert_eq!(store.active_run("p", "F").unwrap(), None);
}

#[test]
fn discard_keep_a_dirty_worktree_and_its_branch_are_kept_while_the_run_is_forgotten() {
    for (tag, staged) in [("untracked", false), ("staged", true)] {
        let repo = GitFixture::new();
        let (root, worktree, store) = paused_run(&format!("dirty-{tag}"), &repo);
        std::fs::write(worktree.join("work.txt"), "unfinished").unwrap();
        if staged {
            Command::new("git").arg("-C").arg(&worktree).args(["add", "work.txt"]).status().unwrap();
        }

        let outcome = discard_run(&store, "run-1", repo.root()).unwrap();

        assert_eq!(outcome.kept_worktree.as_deref(), Some(worktree.as_path()), "{tag}");
        assert_eq!(std::fs::read_to_string(worktree.join("work.txt")).unwrap(), "unfinished", "{tag}");
        assert!(branch_exists(repo.root(), "orq-run-1"), "{tag}");
        assert!(store.load("run-1").is_err(), "{tag}");
        assert!(!root.join("logs").join("run-1").exists(), "{tag}");
        assert_eq!(store.active_run("p", "F").unwrap(), None, "{tag}");
    }
}

#[test]
fn discard_keep_a_run_whose_pause_is_only_requested_is_still_live_and_left_untouched() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("orq-run-1");
    let (_, store) = store("pause-requested");
    store.start_run(&record(&worktree, "orq-run-1")).unwrap();
    store.append_event(&event("run.pause_requested", serde_json::json!({}))).unwrap();

    assert!(discard_run(&store, "run-1", repo.root()).is_err());
    assert!(store.load("run-1").is_ok() && worktree.exists() && branch_exists(repo.root(), "orq-run-1"));
}

fn git_in(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success(), "git {args:?}");
}

#[test]
fn discard_status_untracked_work_is_kept_even_when_git_is_configured_to_hide_it() {
    let repo = GitFixture::new();
    let (_, worktree, store) = paused_run("status-config", &repo);
    git_in(repo.root(), &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(worktree.join("new.txt"), "unfinished").unwrap();

    let outcome = discard_run(&store, "run-1", repo.root()).unwrap();

    assert_eq!(outcome.kept_worktree.as_deref(), Some(worktree.as_path()));
    assert!(worktree.join("new.txt").exists() && branch_exists(repo.root(), "orq-run-1"));
}

#[test]
fn discard_status_a_worktree_whose_cleanliness_cannot_be_told_is_left_as_it_was() {
    let repo = GitFixture::new();
    let (root, worktree, store) = paused_run("status-fails", &repo);
    let gitdir = Command::new("git").arg("-C").arg(&worktree).args(["rev-parse", "--absolute-git-dir"]).output().unwrap();
    let gitdir = PathBuf::from(String::from_utf8(gitdir.stdout).unwrap().trim());
    std::fs::write(gitdir.join("index"), "not an index").unwrap();

    assert!(discard_run(&store, "run-1", repo.root()).is_err());

    assert!(worktree.exists() && branch_exists(repo.root(), "orq-run-1"));
    assert!(store.load("run-1").is_ok() && root.join("logs").join("run-1").exists());
    assert_eq!(store.active_run("p", "F").unwrap().as_deref(), Some("run-1"));
}

#[test]
fn discard_race_work_written_after_the_check_is_kept_with_its_worktree_and_branch() {
    let repo = GitFixture::new();
    let (_, worktree, store) = paused_run("race", &repo);

    let outcome = discard_run_before_remove(&store, "run-1", repo.root(), &mut |_, _| {}, &mut || {
        std::fs::write(worktree.join("late.txt"), "written while the discard ran").unwrap();
    })
    .unwrap();

    assert_eq!(outcome.kept_worktree.as_deref(), Some(worktree.as_path()));
    assert!(worktree.join("late.txt").exists() && branch_exists(repo.root(), "orq-run-1"));
    assert!(store.load("run-1").is_err());
    assert_eq!(store.active_run("p", "F").unwrap(), None);
}

#[test]
fn discard_submodule_a_worktree_with_an_initialized_submodule_is_kept_and_the_run_still_discarded() {
    for (tag, dirty) in [("clean", false), ("dirty", true)] {
        let repo = GitFixture::new();
        let (root, worktree, store) = paused_run(&format!("submodule-{tag}"), &repo);
        let sub = root.join("sub-origin");
        std::fs::create_dir_all(&sub).unwrap();
        git_in(&sub, &["init", "-q"]);
        git_in(&sub, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "sub"]);
        git_in(&worktree, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sub"]);
        git_in(&worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "add sub"]);
        if dirty {
            std::fs::write(worktree.join("sub").join("work.txt"), "unfinished").unwrap();
        }

        let outcome = discard_run(&store, "run-1", repo.root()).unwrap();

        assert_eq!(outcome.kept_worktree.as_deref(), Some(worktree.as_path()), "{tag}");
        assert!(worktree.join("sub").exists() && branch_exists(repo.root(), "orq-run-1"), "{tag}");
        assert!(store.load("run-1").is_err(), "{tag}");
        assert_eq!(store.active_run("p", "F").unwrap(), None, "{tag}");
    }
}

fn current_branch(repo: &Path) -> String {
    let out = Command::new("git").arg("-C").arg(repo).args(["symbolic-ref", "--short", "HEAD"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Commits `count` new files in `worktree` and returns the last commit's SHA.
fn commit_ahead(worktree: &Path, count: usize) -> String {
    for n in 0..count {
        std::fs::write(worktree.join(format!("ahead-{n}.txt")), "committed work\n").unwrap();
        git_in(worktree, &["add", "."]);
        git_in(worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", &format!("ahead {n}")]);
    }
    head_sha(worktree)
}

/// A failed run whose clean worktree's branch is `ahead` commits past the repository's own branch.
fn failed_run_ahead(tag: &str, repo: &GitFixture, ahead: usize) -> (PathBuf, String, RunStore) {
    let worktree = repo.add_linked_worktree("orq-run-1");
    let last = commit_ahead(&worktree, ahead);
    let (_, store) = store(tag);
    let mut rec = record(&worktree, "orq-run-1");
    rec.target_branch = current_branch(repo.root());
    store.start_run(&rec).unwrap();
    store.append_event(&event("run.failed", serde_json::json!({}))).unwrap();
    (worktree, last, store)
}

#[test]
fn discard_ahead_a_deleted_branch_is_reported_with_its_commits_past_the_target_and_its_last_commit() {
    let repo = GitFixture::new();
    let (_, last, store) = failed_run_ahead("ahead-outcome", &repo, 2);

    let outcome = discard_run(&store, "run-1", repo.root()).unwrap();

    let deleted = outcome.deleted_branch.expect("the branch was deleted");
    assert_eq!((deleted.name.as_str(), deleted.commits_ahead, deleted.last_commit.as_str()), ("orq-run-1", Some(2), last.as_str()));
    assert!(!branch_exists(repo.root(), "orq-run-1"));
}

#[test]
fn discard_ahead_the_preview_names_the_commits_a_discard_would_delete_and_changes_nothing() {
    for (tag, ahead, dirty, expected) in [("two", 2, false, Some(2)), ("none", 0, false, Some(0)), ("dirty", 2, true, None)] {
        let repo = GitFixture::new();
        let (worktree, last, store) = failed_run_ahead(&format!("preview-{tag}"), &repo, ahead);
        if dirty {
            std::fs::write(worktree.join("work.txt"), "unfinished").unwrap();
        }

        let preview = discard_preview(&store, "run-1", repo.root()).unwrap();

        match expected {
            Some(count) => {
                let branch = preview.unwrap_or_else(|| panic!("{tag}: the branch would go"));
                assert_eq!((branch.commits_ahead, branch.last_commit.as_str()), (Some(count), last.as_str()), "{tag}");
            }
            None => assert_eq!(preview, None, "{tag}: a dirty worktree keeps its branch"),
        }
        assert!(worktree.exists() && branch_exists(repo.root(), "orq-run-1") && store.load("run-1").is_ok(), "{tag}");
    }
}
