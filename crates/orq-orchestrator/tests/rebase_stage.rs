use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    worktree: PathBuf,
    /// A second clone used to move the target branch on the remote.
    other: PathBuf,
}

/// A bare remote with `main`, a worktree on `fix--COM-02` with one commit of
/// its own, and another clone to advance `main`.
fn world(tag: &str, remote_override: Option<&str>) -> World {
    let root = std::env::temp_dir().join(format!("orq-rebase-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    let worktree = root.join("worktree");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), worktree.to_str().unwrap()]);
    // The rebase itself commits, so the identity must live in the repository,
    // not only in the helper's -c flags.
    for (key, value) in [("user.name", "t"), ("user.email", "t@example.com"), ("commit.gpgsign", "false")] {
        git(&worktree, &["config", key, value]);
    }
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    git(&worktree, &["push", "-q", "origin", "HEAD:main"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["checkout", "-q", "-b", "fix--COM-02"]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);
    let other = root.join("other");
    git(&root, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);

    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
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
    World { store, worktree, other }
}

fn advance_target(w: &World, file: &str, content: &str) -> String {
    advance_target_on(w, "main", file, content)
}

/// Commit in the other clone and push it as `branch` on the remote.
fn advance_target_on(w: &World, branch: &str, file: &str, content: &str) -> String {
    fs::write(w.other.join(file), content).unwrap();
    git(&w.other, &["add", "."]);
    git(&w.other, &["commit", "-q", "-m", &format!("target {file}")]);
    git(&w.other, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
    git(&w.other, &["rev-parse", "HEAD"])
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn the_run_branch_is_rebased_onto_the_fetched_target_and_its_sha_recorded() {
    let w = world("ok", None);
    let target = advance_target(&w, "theirs.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: target.clone() });
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD~1"]), target, "the run's commit sits on the target");
    assert!(w.worktree.join("theirs.txt").is_file() && w.worktree.join("mine.txt").is_file());
    let events = w.store.events("run-1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "rebase.completed");
    assert_eq!(events[0].stage.as_deref(), Some("rebase"));
    assert_eq!(events[0].payload["target_sha"], target.as_str());
}

#[test]
fn a_conflict_blocks_the_run_and_leaves_the_worktree_mid_rebase() {
    let w = world("conflict", None);
    fs::write(w.worktree.join("a.txt"), "mine\n").unwrap();
    git(&w.worktree, &["commit", "-q", "-am", "edit a"]);
    advance_target(&w, "a.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    let RebaseOutcome::Conflict { reason } = outcome else { panic!("expected conflict, got {outcome:?}") };
    assert!(reason.contains("conflict"), "{reason}");
    assert!(git(&w.worktree, &["status", "--porcelain=v1"]).contains("UU a.txt"), "conflict is left to resolve");
    let events = w.store.events("run-1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.blocked");
    assert_eq!(events[0].stage.as_deref(), Some("rebase"));
    assert_eq!(events[0].payload["reason"], reason.as_str());
}

#[test]
fn a_disabled_rebase_changes_nothing_and_says_so() {
    let w = world("disabled", None);
    advance_target(&w, "theirs.txt", "theirs\n");
    let before = git(&w.worktree, &["rev-parse", "HEAD"]);

    let outcome = rebase_onto_target(&w.store, "run-1", false).unwrap();

    assert_eq!(outcome, RebaseOutcome::Skipped);
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), before);
    assert_eq!(kinds(&w), ["rebase.skipped"]);
}

#[test]
fn a_remote_that_cannot_be_fetched_is_an_error_and_records_nothing() {
    let w = world("badremote", Some("/nonexistent/orq-remote.git"));

    assert!(rebase_onto_target(&w.store, "run-1", true).is_err());
    assert!(kinds(&w).is_empty());
}

#[test]
fn an_unknown_run_is_an_error() {
    let w = world("unknown", None);
    assert!(rebase_onto_target(&w.store, "ghost", true).is_err());
}

#[test]
fn a_worktree_on_another_branch_is_refused_without_touching_it() {
    let w = world("wrong-branch", None);
    advance_target(&w, "theirs.txt", "theirs\n");
    git(&w.worktree, &["checkout", "-q", "-b", "someone-else"]);
    let before = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert!(rebase_onto_target(&w.store, "run-1", true).is_err());

    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&w.worktree, &["symbolic-ref", "HEAD"]), "refs/heads/someone-else");
    assert!(kinds(&w).is_empty());
}

#[test]
fn a_detached_head_is_refused_without_touching_it() {
    let w = world("detached", None);
    advance_target(&w, "theirs.txt", "theirs\n");
    git(&w.worktree, &["checkout", "-q", "--detach"]);
    let before = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert!(rebase_onto_target(&w.store, "run-1", true).is_err());

    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), before);
    assert!(kinds(&w).is_empty());
}

#[test]
fn the_fetched_target_is_kept_in_a_run_specific_ref_that_is_removed_afterwards() {
    let w = world("fetch-ref", None);
    advance_target(&w, "theirs.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert!(matches!(outcome, RebaseOutcome::Rebased { .. }));
    assert_eq!(git(&w.worktree, &["for-each-ref", "refs/orq"]), "", "no leftover ref");
}

#[test]
fn the_run_specific_ref_is_removed_after_a_conflict_too() {
    let w = world("fetch-ref-conflict", None);
    fs::write(w.worktree.join("a.txt"), "mine\n").unwrap();
    git(&w.worktree, &["commit", "-q", "-am", "edit a"]);
    advance_target(&w, "a.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert!(matches!(outcome, RebaseOutcome::Conflict { .. }));
    assert_eq!(git(&w.worktree, &["for-each-ref", "refs/orq"]), "");
}

#[test]
fn the_shared_fetch_head_is_left_alone_so_another_runs_fetch_is_not_clobbered() {
    let w = world("fetch-head", None);
    advance_target(&w, "theirs.txt", "theirs\n");
    // Another run's fetch in the same repository leaves its result in FETCH_HEAD.
    git(&w.worktree, &["fetch", "-q", "origin", "main"]);
    let git_dir = PathBuf::from(git(&w.worktree, &["rev-parse", "--absolute-git-dir"]));
    let before = fs::read_to_string(git_dir.join("FETCH_HEAD")).unwrap();
    advance_target(&w, "later.txt", "later\n");

    rebase_onto_target(&w.store, "run-1", true).unwrap();

    assert_eq!(fs::read_to_string(git_dir.join("FETCH_HEAD")).unwrap(), before);
}

#[test]
fn a_rebase_that_fails_without_a_conflict_is_an_error_and_keeps_local_work() {
    let w = world("nonconflict", None);
    advance_target(&w, "mine.txt", "theirs\n");
    // Uncommitted edit to a file the rebase has to touch, with autostash off.
    fs::write(w.worktree.join("mine.txt"), "dirty\n").unwrap();
    git(&w.worktree, &["config", "rebase.autoStash", "false"]);
    let before = git(&w.worktree, &["rev-parse", "HEAD"]);

    assert!(rebase_onto_target(&w.store, "run-1", true).is_err());

    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), before);
    assert_eq!(fs::read_to_string(w.worktree.join("mine.txt")).unwrap(), "dirty\n");
    assert!(kinds(&w).is_empty(), "no success or conflict event");
}

#[test]
fn run_ids_that_git_would_reject_as_ref_names_still_rebase() {
    for id in ["run..1", "run.lock", "run."] {
        let w = world(&format!("refname-{}", id.replace('.', "_")), None);
        let mut record = w.store.load("run-1").unwrap();
        record.run_id = id.to_string();
        w.store.create(&record).unwrap();
        let target = advance_target(&w, "theirs.txt", "theirs\n");

        let outcome = rebase_onto_target(&w.store, id, true).unwrap();

        assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: target }, "run id {id}");
        assert_eq!(git(&w.worktree, &["for-each-ref", "refs/orq"]), "", "run id {id}");
    }
}

#[test]
fn a_run_id_of_the_longest_length_still_rebases_and_leaves_no_ref() {
    let id = "r".repeat(128);
    let w = world("longid", None);
    let mut record = w.store.load("run-1").unwrap();
    record.run_id = id.clone();
    w.store.create(&record).unwrap();
    let target = advance_target(&w, "theirs.txt", "theirs\n");

    let outcome = rebase_onto_target(&w.store, &id, true).unwrap();

    assert_eq!(outcome, RebaseOutcome::Rebased { target_sha: target });
    assert_eq!(git(&w.worktree, &["for-each-ref", "refs/orq"]), "");
}

#[test]
fn two_linked_worktrees_each_rebase_onto_their_own_target_and_clean_only_their_own_ref() {
    let w = world("linked", None);
    // Run 2 lives in a linked worktree of the same repository and targets another branch.
    git(&w.other, &["checkout", "-q", "-b", "release"]);
    let release = advance_target_on(&w, "release", "release.txt", "release\n");
    let main = advance_target(&w, "main.txt", "main\n");
    let second = w.worktree.parent().unwrap().join("second");
    git(&w.worktree, &["worktree", "add", "-q", "-b", "fix--COM-03", second.to_str().unwrap(), "HEAD"]);
    let mut record = w.store.load("run-1").unwrap();
    record.run_id = "run-2".to_string();
    record.feature_id = "COM-03".to_string();
    record.branch = "fix--COM-03".to_string();
    record.target_branch = "release".to_string();
    record.worktree = second.clone();
    w.store.create(&record).unwrap();
    // Run 1's fetch ref is still in the shared repository while run 2 works.
    let first_ref = orq_orchestrator::private_ref_name("run-1");
    git(&w.worktree, &["update-ref", &first_ref, "HEAD"]);

    let second_outcome = rebase_onto_target(&w.store, "run-2", true).unwrap();
    assert_eq!(second_outcome, RebaseOutcome::Rebased { target_sha: release });
    assert!(git(&w.worktree, &["for-each-ref", "refs/orq"]).contains(&first_ref), "run 1's ref is untouched");
    git(&w.worktree, &["update-ref", "-d", &first_ref]);

    let first_outcome = rebase_onto_target(&w.store, "run-1", true).unwrap();
    assert_eq!(first_outcome, RebaseOutcome::Rebased { target_sha: main });
}
