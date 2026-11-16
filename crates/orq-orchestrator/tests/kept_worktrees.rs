use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_orchestrator::{
    delete_kept_worktree, list_kept_worktrees, ChangeKind, DeleteRefusal, Event, RunIndex, RunRecord, RunStore,
};

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
    root: PathBuf,
    store: RunStore,
    repo: PathBuf,
}

/// A repository on `main` that ignores `.claude/settings.local.json` and its worktrees folder.
fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-kept-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n.claude/worktrees/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let store = RunStore::new(&root.join("state"));
    World { root, store, repo }
}

impl World {
    /// A worktree on `branch` under the repository's worktrees folder, recorded as run `run_id`.
    fn worktree(&self, run_id: &str, branch: &str) -> PathBuf {
        let path = self.repo.join(".claude/worktrees").join(branch);
        git(&self.repo, &["worktree", "add", "-q", "-b", branch, path.to_str().unwrap()]);
        self.record(run_id, branch, &path);
        path
    }

    fn record(&self, run_id: &str, branch: &str, path: &Path) {
        let base = git(&self.repo, &["rev-parse", "main"]);
        self.store
            .create(&RunRecord {
                delivery: Default::default(),
                run_id: run_id.to_string(),
                project_id: "repo-1".to_string(),
                feature_id: branch.trim_start_matches("fix--").to_string(),
                remote: "origin".to_string(),
                provider: "github".to_string(),
                target_branch: "main".to_string(),
                base_sha: base,
                branch: branch.to_string(),
                worktree: path.to_path_buf(),
                repository: String::new(),
                profiles: BTreeMap::new(),
            })
            .unwrap();
    }

    fn event(&self, run_id: &str, kind: &str, payload: serde_json::Value) {
        let record = self.store.load(run_id).unwrap();
        let n = self.store.events(run_id).unwrap().len();
        self.store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run_id}-{n}"),
                run_id: run_id.to_string(),
                project_id: record.project_id,
                feature_id: record.feature_id,
                kind: kind.to_string(),
                stage: Some("cleanup".to_string()),
                attempt: 1,
                occurred_at: "2026-11-16T00:00:00Z".to_string(),
                payload,
            })
            .unwrap();
    }

    /// The run ended and cleanup kept its worktree for `reason`.
    fn kept(&self, run_id: &str, reason: &str) {
        let worktree = self.store.load(run_id).unwrap().worktree;
        self.event(
            run_id,
            "worktree.kept",
            serde_json::json!({ "worktree": worktree, "reason": reason, "run_succeeded": true }),
        );
    }
}

#[test]
fn a_merged_worktree_with_only_an_ignored_settings_file_is_listed_as_safe_to_delete() {
    let w = world("merged");
    let path = w.worktree("run-1", "fix--A");
    fs::create_dir_all(path.join(".claude")).unwrap();
    fs::write(path.join(".claude/settings.local.json"), "{}\n").unwrap();
    w.kept("run-1", "the worktree has uncommitted changes: .claude/settings.local.json");

    let kept = list_kept_worktrees(&w.store, &w.repo).unwrap();

    assert_eq!(kept.len(), 1, "{kept:?}");
    let entry = &kept[0];
    assert_eq!(entry.branch, "fix--A");
    assert_eq!(entry.run_id.as_deref(), Some("run-1"));
    assert_eq!(entry.ending.as_deref(), Some("completed"));
    assert_eq!(
        entry.kept_reason.as_deref(),
        Some("the worktree has uncommitted changes: .claude/settings.local.json")
    );
    assert_eq!(entry.commits, Some(0));
    assert_eq!(entry.files.len(), 1);
    assert_eq!(entry.files[0].path, ".claude/settings.local.json");
    assert_eq!(entry.files[0].kind, ChangeKind::Ignored);
    assert_eq!(entry.more_files, 0);
    assert!(entry.merged() && entry.safe(), "{entry:?}");
    assert!(entry.size.is_some_and(|bytes| bytes > 0));
    assert!(!entry.version.is_empty());
    let _ = &w.root;
}

#[test]
fn a_worktree_with_an_unmerged_commit_or_an_untracked_file_is_listed_but_not_safe() {
    let w = world("unmerged");
    let ahead = w.worktree("run-1", "fix--B");
    fs::write(ahead.join("b.txt"), "b\n").unwrap();
    git(&ahead, &["add", "."]);
    git(&ahead, &["commit", "-q", "-m", "b"]);
    let untracked = w.worktree("run-2", "fix--C");
    fs::write(untracked.join("notes.txt"), "mine\n").unwrap();
    for run in ["run-1", "run-2"] {
        w.kept(run, "kept");
    }

    let kept = list_kept_worktrees(&w.store, &w.repo).unwrap();

    let branches: Vec<&str> = kept.iter().map(|k| k.branch.as_str()).collect();
    assert_eq!(branches, ["fix--B", "fix--C"]);
    assert_eq!(kept[0].commits, Some(1));
    assert!(kept[0].files.is_empty());
    assert!(!kept[0].merged() && !kept[0].safe());
    assert_eq!(kept[1].commits, Some(0));
    assert_eq!(kept[1].files, [orq_orchestrator::ChangedFile { path: "notes.txt".to_string(), kind: ChangeKind::Untracked }]);
    assert!(kept[1].merged() && !kept[1].safe());
}

#[test]
fn the_main_worktree_one_outside_the_folder_and_an_active_runs_are_not_listed() {
    let w = world("hidden");
    let outside = w.root.join("elsewhere");
    git(&w.repo, &["worktree", "add", "-q", "-b", "fix--Out", outside.to_str().unwrap()]);
    w.record("run-1", "fix--Out", &outside);
    w.kept("run-1", "kept");
    w.worktree("run-2", "fix--Busy");
    w.event("run-2", "stage.started", serde_json::json!({}));
    w.worktree("run-3", "fix--Done");
    w.kept("run-3", "kept");

    let kept = list_kept_worktrees(&w.store, &w.repo).unwrap();

    let branches: Vec<&str> = kept.iter().map(|k| k.branch.as_str()).collect();
    assert_eq!(branches, ["fix--Done"]);
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn deleting_a_merged_worktree_removes_it_and_its_branch_and_records_the_sha() {
    let w = world("delete");
    let path = w.worktree("run-1", "fix--A");
    fs::create_dir_all(path.join(".claude")).unwrap();
    fs::write(path.join(".claude/settings.local.json"), "{}\n").unwrap();
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);

    let deleted = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, false).unwrap().unwrap();

    assert_eq!(deleted.sha, entry.head);
    assert_eq!(deleted.run_id.as_deref(), Some("run-1"));
    assert!(!path.exists());
    assert!(!branch_exists(&w.repo, "fix--A"));
    assert!(!git(&w.repo, &["worktree", "list", "--porcelain"]).contains("fix--A"));
    let events = w.store.events("run-1").unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.kind, "worktree.deleted");
    assert_eq!(last.payload["sha"], entry.head);
    assert_eq!(last.payload["branch"], "fix--A");
    assert!(list_kept_worktrees(&w.store, &w.repo).unwrap().is_empty());
}

#[test]
fn a_stale_version_is_a_conflict_that_deletes_nothing() {
    let w = world("stale");
    let path = w.worktree("run-1", "fix--A");
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    fs::write(path.join("late.txt"), "new\n").unwrap();

    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();

    assert_eq!(refused, DeleteRefusal::Changed);
    assert_eq!(refused.to_string(), "the worktree changed since you opened it; nothing was deleted");
    assert!(path.join("late.txt").exists());
    assert!(branch_exists(&w.repo, "fix--A"));
}

#[test]
fn an_unmerged_worktree_is_deleted_only_with_force() {
    let w = world("force");
    let path = w.worktree("run-1", "fix--B");
    fs::write(path.join("b.txt"), "b\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "b"]);
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);

    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--B", &entry.version, false).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::NeedsForce);
    assert!(path.exists() && branch_exists(&w.repo, "fix--B"));

    delete_kept_worktree(&w.store, &w.repo, "fix--B", &entry.version, true).unwrap().unwrap();
    assert!(!path.exists() && !branch_exists(&w.repo, "fix--B"));
}

#[cfg(unix)]
#[test]
fn a_worktree_reached_through_a_symlink_is_refused() {
    let w = world("symlink");
    let real = w.root.join("real-worktrees");
    fs::create_dir_all(&real).unwrap();
    fs::create_dir_all(w.repo.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&real, w.repo.join(".claude/worktrees")).unwrap();
    let path = w.repo.join(".claude/worktrees/fix--L");
    git(&w.repo, &["worktree", "add", "-q", "-b", "fix--L", path.to_str().unwrap()]);
    w.record("run-1", "fix--L", &path);
    w.kept("run-1", "kept");

    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--L", "any", true).unwrap().unwrap_err();

    assert!(matches!(refused, DeleteRefusal::NotFound | DeleteRefusal::Unsafe(_)), "{refused:?}");
    assert!(real.join("fix--L").exists());
    assert!(branch_exists(&w.repo, "fix--L"));
}

#[test]
fn an_unknown_branch_is_not_found() {
    let w = world("unknown");
    assert_eq!(
        delete_kept_worktree(&w.store, &w.repo, "fix--Nope", "v", true).unwrap().unwrap_err(),
        DeleteRefusal::NotFound
    );
}

#[test]
fn a_run_of_another_repository_on_the_same_branch_says_nothing_about_this_worktree() {
    let w = world("two-repos");
    w.worktree("run-1", "fix--A");
    w.kept("run-1", "the worktree has uncommitted changes");
    let other = w.root.join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "release"]);
    fs::write(other.join("a.txt"), "a\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "base"]);
    let elsewhere = other.join(".claude/worktrees/fix--A");
    git(&other, &["worktree", "add", "-q", "-b", "fix--A", elsewhere.to_str().unwrap()]);
    w.store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-2".to_string(),
            project_id: "repo-2".to_string(),
            feature_id: "B".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "release".to_string(),
            base_sha: git(&other, &["rev-parse", "release"]),
            branch: "fix--A".to_string(),
            worktree: elsewhere,
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    // run-2 is still running in the other repository.
    w.event("run-2", "stage.started", serde_json::Value::Null);

    let kept = list_kept_worktrees(&w.store, &w.repo).unwrap();

    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].run_id.as_deref(), Some("run-1"));
    assert_eq!(kept[0].target, "main");
    let deleted = delete_kept_worktree(&w.store, &w.repo, "fix--A", &kept[0].version, false).unwrap().unwrap();
    assert_eq!(deleted.run_id.as_deref(), Some("run-1"));
    assert!(w.store.events("run-2").unwrap().iter().all(|e| e.kind != "worktree.deleted"));
}

#[test]
fn editing_an_already_changed_file_or_one_past_the_shown_files_makes_the_version_stale() {
    let w = world("stale-content");
    let path = w.worktree("run-1", "fix--A");
    for n in 0..25 {
        fs::write(path.join(format!("f{n:02}.txt")), "first\n").unwrap();
    }
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    assert_eq!(entry.files.len(), 20);

    fs::write(path.join("f00.txt"), "second, longer\n").unwrap();
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::Changed);

    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    fs::remove_file(path.join("f24.txt")).unwrap();
    fs::write(path.join("f99.txt"), "first\n").unwrap();
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::Changed);
    assert!(path.join("f99.txt").exists() && branch_exists(&w.repo, "fix--A"));
}

/// Listing reads the events only of the runs recorded on a listed worktree: an unrelated run's history,
/// here a pipe nothing writes to, is never opened.
#[cfg(unix)]
#[test]
fn listing_and_deleting_read_no_events_of_unrelated_runs() {
    let w = world("unrelated-events");
    w.worktree("run-1", "fix--A");
    w.kept("run-1", "kept");
    w.record("run-9", "fix--elsewhere", &w.root.join("other/.claude/worktrees/fix--elsewhere"));
    let events = w.root.join("state/runs/run-9/events.jsonl");
    let _ = fs::remove_file(&events);
    assert!(Command::new("mkfifo").arg(&events).status().unwrap().success());

    let (store, repo) = (RunStore::new(&w.root.join("state")), w.repo.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let kept = list_kept_worktrees(&store, &repo).unwrap();
        let deleted = delete_kept_worktree(&store, &repo, "fix--A", &kept[0].version, false).unwrap();
        let _ = tx.send((kept.len(), deleted.is_ok()));
    });
    let outcome = rx.recv_timeout(std::time::Duration::from_secs(20));
    if outcome.is_err() {
        // Let a reader blocked on the pipe go.
        let _ = fs::OpenOptions::new().write(true).open(&events);
    }
    assert_eq!(outcome.expect("listing waited on an unrelated run's events"), (1, true));
}

#[test]
fn a_branch_git_cannot_delete_after_the_worktree_went_is_an_error_saying_so() {
    let w = world("branch-locked");
    let path = w.worktree("run-1", "fix--A");
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    fs::write(w.repo.join(".git/refs/heads/fix--A.lock"), "").unwrap();

    let err = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, false).unwrap_err();

    assert!(err.to_string().contains("the worktree was removed but git could not delete its branch"), "{err}");
    assert!(!path.exists());
    assert!(branch_exists(&w.repo, "fix--A"));
}

/// The worktree and its branch are gone once git says so; a deletion event that cannot be written
/// does not make that an error.
#[cfg(unix)]
#[test]
fn a_deletion_event_that_cannot_be_written_still_reports_the_deletion() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("event-unwritable");
    let path = w.worktree("run-1", "fix--A");
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    let events = w.root.join("state/runs/run-1/events.jsonl");
    fs::set_permissions(&events, fs::Permissions::from_mode(0o444)).unwrap();
    if fs::OpenOptions::new().append(true).open(&events).is_ok() {
        return; // Running as a user permissions do not stop.
    }

    let deleted = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, false).unwrap().unwrap();

    assert_eq!(deleted.run_id.as_deref(), Some("run-1"));
    assert!(!path.exists() && !branch_exists(&w.repo, "fix--A"));
}

/// Git lists a nested repository as one directory; editing a file inside it still makes the version stale.
#[test]
fn editing_a_file_inside_a_nested_repository_makes_the_version_stale() {
    let w = world("stale-nested");
    let path = w.worktree("run-1", "fix--A");
    let sub = path.join("sub");
    fs::create_dir_all(sub.join("deep")).unwrap();
    git(&sub, &["init", "-q"]);
    fs::write(sub.join("deep/x.txt"), "first\n").unwrap();
    git(&sub, &["add", "."]);
    git(&sub, &["commit", "-q", "-m", "x"]);
    w.kept("run-1", "kept");
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    assert!(entry.files.iter().any(|f| f.path.trim_end_matches('/') == "sub"), "{:?}", entry.files);

    fs::write(sub.join("deep/x.txt"), "second, longer\n").unwrap();
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::Changed);
    assert!(sub.join("deep/x.txt").exists() && branch_exists(&w.repo, "fix--A"));
}

/// `path` gets `text`, which is as long as what it held, and its old modification time back.
fn rewrite_keeping_size_and_time(path: &Path, text: &str) {
    let before = fs::metadata(path).unwrap();
    assert_eq!(before.len(), text.len() as u64);
    fs::write(path, text).unwrap();
    fs::File::options().write(true).open(path).unwrap().set_modified(before.modified().unwrap()).unwrap();
    let after = fs::metadata(path).unwrap();
    assert_eq!((after.len(), after.modified().unwrap()), (before.len(), before.modified().unwrap()));
}

/// A tool that keeps a file's size and modification time still changes what is in it.
#[test]
fn rewriting_a_file_with_its_size_and_time_kept_makes_the_version_stale() {
    let w = world("stale-same-size");
    let path = w.worktree("run-1", "fix--A");
    fs::write(path.join("top.txt"), "first\n").unwrap();
    let sub = path.join("sub");
    fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    fs::write(sub.join("x.txt"), "first\n").unwrap();
    w.kept("run-1", "kept");

    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    rewrite_keeping_size_and_time(&path.join("top.txt"), "other\n");
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::Changed);

    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    rewrite_keeping_size_and_time(&sub.join("x.txt"), "third\n");
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap().unwrap_err();
    assert_eq!(refused, DeleteRefusal::Changed);
    assert!(sub.join("x.txt").exists() && branch_exists(&w.repo, "fix--A"));
}

/// A version that ran out of time before reading everything cannot tell a change from none, so a delete
/// confirmed against it is refused, even forced and even when the listing ran out of time the same way.
/// A file or folder the version cannot read could change unseen, so it cannot confirm a delete.
#[cfg(unix)]
#[test]
fn a_version_with_something_it_cannot_read_deletes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let w = world("version-unreadable");
    let path = w.worktree("run-1", "fix--A");
    let sub = path.join("sub");
    fs::create_dir_all(sub.join("locked")).unwrap();
    git(&sub, &["init", "-q"]);
    fs::write(sub.join("secret.txt"), "work\n").unwrap();
    fs::write(sub.join("locked/inner.txt"), "work\n").unwrap();
    w.kept("run-1", "kept");
    let set = |p: &std::path::Path, mode| fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();

    set(&sub.join("secret.txt"), 0o000);
    if fs::read(sub.join("secret.txt")).is_ok() {
        return; // Running as a user permissions do not stop.
    }
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap();
    assert_eq!(refused, Err(DeleteRefusal::Unverifiable));
    set(&sub.join("secret.txt"), 0o644);

    set(&sub.join("locked"), 0o000);
    let entry = list_kept_worktrees(&w.store, &w.repo).unwrap().remove(0);
    let refused = delete_kept_worktree(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap();
    set(&sub.join("locked"), 0o755);
    assert_eq!(refused, Err(DeleteRefusal::Unverifiable));
    assert!(sub.join("locked/inner.txt").exists() && branch_exists(&w.repo, "fix--A"));
}

#[test]
fn a_version_read_only_in_part_before_its_time_ran_out_deletes_nothing() {
    let w = world("version-timeout");
    let path = w.worktree("run-1", "fix--A");
    fs::write(path.join("work.txt"), "work\n").unwrap();
    w.kept("run-1", "kept");
    let mut index = RunIndex::load(&w.store).unwrap().with_version_budget(Duration::ZERO);

    let entry = index.list(&w.store, &w.repo).unwrap().remove(0);
    let refused = index.delete(&w.store, &w.repo, "fix--A", &entry.version, true).unwrap();

    assert_eq!(refused, Err(DeleteRefusal::Unverifiable));
    assert!(path.join("work.txt").exists() && branch_exists(&w.repo, "fix--A"));
}

#[test]
fn one_run_index_serves_several_listings_and_deletes_and_still_sees_a_run_started_after_it_was_read() {
    let w = world("index");
    let a = w.worktree("run-1", "fix--A");
    w.kept("run-1", "kept");
    let b = w.worktree("run-2", "fix--B");
    w.kept("run-2", "kept");
    let mut index = RunIndex::load(&w.store).unwrap();

    let listed = index.list(&w.store, &w.repo).unwrap();
    assert_eq!(listed.iter().map(|k| k.branch.as_str()).collect::<Vec<_>>(), ["fix--A", "fix--B"]);
    let version = |branch: &str| listed.iter().find(|k| k.branch == branch).unwrap().version.clone();

    // A run that starts on fix--B after the index was read still holds it.
    w.record("run-3", "fix--B", &b);
    w.event("run-3", "stage.started", serde_json::json!({}));

    let deleted = index.delete(&w.store, &w.repo, "fix--A", &version("fix--A"), false).unwrap();
    assert!(deleted.is_ok(), "{deleted:?}");
    assert!(!a.exists());
    let refused = index.delete(&w.store, &w.repo, "fix--B", &version("fix--B"), false).unwrap();
    assert_eq!(refused, Err(DeleteRefusal::Active));
    assert!(b.exists());
}
