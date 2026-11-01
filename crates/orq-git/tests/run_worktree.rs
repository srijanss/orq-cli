use std::path::{Path, PathBuf};
use std::process::Command;

use orq_git::{create_run_worktree, WorktreeError};
use orq_test_support::GitFixture;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A repository whose HEAD has moved on from the recorded base commit, with an
/// uncommitted change in the source checkout.
fn moved_on() -> (GitFixture, String) {
    let fixture = GitFixture::new();
    let root = fixture.root();
    let base = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("later.txt"), "later\n").unwrap();
    git(root, &["add", "later.txt"]);
    git(root, &["commit", "--quiet", "-m", "later"]);
    std::fs::write(root.join("README.md"), "dirty\n").unwrap();
    (fixture, base)
}

#[test]
fn the_worktree_and_branch_start_at_the_base_sha_and_leave_the_source_checkout_alone() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    let head_before = git(root, &["rev-parse", "HEAD"]);
    let branch_before = git(root, &["symbolic-ref", "HEAD"]);

    let dir = Path::new(".claude/worktrees/fix--a");
    let path = create_run_worktree(root, dir, "fix--a", &base).unwrap();

    assert_eq!(path, root.join(dir));
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&path, &["symbolic-ref", "--short", "HEAD"]), "fix--a");
    assert!(!path.join("later.txt").exists(), "the worktree is at the base, not the current HEAD");
    // No upstream is configured for the new branch.
    assert!(Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--abbrev-ref", "fix--a@{upstream}"])
        .output()
        .map(|o| !o.status.success())
        .unwrap());

    // The source checkout keeps its HEAD, branch and uncommitted change.
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(root, &["symbolic-ref", "HEAD"]), branch_before);
    assert_eq!(std::fs::read_to_string(root.join("README.md")).unwrap(), "dirty\n");
    assert_eq!(git(root, &["status", "--porcelain", "--untracked-files=no"]), "M README.md");
}

#[test]
fn a_taken_branch_or_path_is_refused_without_changing_anything() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    git(root, &["branch", "fix--taken", &base]);
    let branches = git(root, &["branch", "--list"]);

    let err = create_run_worktree(root, Path::new(".claude/worktrees/fix--taken"), "fix--taken", &base).unwrap_err();
    assert!(matches!(err, WorktreeError::BranchExists { ref branch } if branch == "fix--taken"), "{err:?}");
    assert!(!root.join(".claude/worktrees/fix--taken").exists());

    // An existing directory (even an empty one) is never adopted.
    std::fs::create_dir_all(root.join(".claude/worktrees/fix--b")).unwrap();
    let err = create_run_worktree(root, Path::new(".claude/worktrees/fix--b"), "fix--b", &base).unwrap_err();
    assert!(matches!(err, WorktreeError::PathExists { .. }), "{err:?}");
    assert_eq!(git(root, &["branch", "--list"]), branches, "no branch was created");

    // A second run on the same name collides on both.
    create_run_worktree(root, Path::new(".claude/worktrees/fix--c"), "fix--c", &base).unwrap();
    assert!(create_run_worktree(root, Path::new(".claude/worktrees/fix--c"), "fix--c", &base).is_err());
}

#[test]
fn unsafe_inputs_are_refused_before_git_is_run() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    let ok_dir = Path::new(".claude/worktrees/fix--x");

    for bad in ["", "-x", "a b", "a..b", "a:b", "x.lock", "--detach"] {
        assert!(
            matches!(create_run_worktree(root, ok_dir, bad, &base), Err(WorktreeError::InvalidName { .. })),
            "branch {bad:?}"
        );
    }
    for bad in ["", "zz", "-1", "HEAD", "main", &"g".repeat(40)] {
        assert!(
            matches!(create_run_worktree(root, ok_dir, "fix--x", bad), Err(WorktreeError::InvalidName { .. })),
            "sha {bad:?}"
        );
    }
    for bad in ["", "/tmp/escape", "../escape", ".claude/../../escape", "a/../../b"] {
        assert!(
            matches!(
                create_run_worktree(root, &PathBuf::from(bad), "fix--x", &base),
                Err(WorktreeError::InvalidName { .. })
            ),
            "dir {bad:?}"
        );
    }
    assert!(!root.join(".claude").exists());
    assert!(!git(root, &["branch", "--list"]).contains("fix--x"));
}

#[test]
fn an_unknown_base_commit_is_reported_and_leaves_no_branch_behind() {
    let (fixture, _) = moved_on();
    let root = fixture.root();
    let missing = "0123456789abcdef0123456789abcdef01234567";

    let err = create_run_worktree(root, Path::new(".claude/worktrees/fix--y"), "fix--y", missing).unwrap_err();
    assert!(matches!(err, WorktreeError::Git { .. }), "{err:?}");
    assert!(!git(root, &["branch", "--list"]).contains("fix--y"));
    assert!(!root.join(".claude/worktrees/fix--y").exists());
}

#[test]
fn a_relative_repository_path_creates_the_worktree_where_the_result_says() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    // The same repository, spelled relative to this process's working directory
    // (no other test here depends on the working directory).
    std::env::set_current_dir(root.parent().unwrap()).unwrap();
    let relative = PathBuf::from(root.file_name().unwrap());
    assert!(relative.is_relative());

    let path = create_run_worktree(&relative, Path::new(".claude/worktrees/fix--rel"), "fix--rel", &base).unwrap();

    assert!(path.join(".git").exists(), "{} is the worktree", path.display());
    assert_eq!(git(&path, &["symbolic-ref", "--short", "HEAD"]), "fix--rel");
    assert!(root.join(".claude/worktrees/fix--rel").is_dir());
    assert!(!root.join(&relative).exists(), "nothing nested under the repository");
}

#[cfg(unix)]
#[test]
fn a_symlinked_parent_cannot_send_the_worktree_outside_the_repository() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    let outside = root.parent().unwrap().join(format!("outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".claude")).unwrap();
    let branches = git(root, &["branch", "--list"]);

    let err = create_run_worktree(root, Path::new(".claude/worktrees/fix--s"), "fix--s", &base).unwrap_err();

    assert!(matches!(err, WorktreeError::InvalidName { .. }), "{err:?}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing written outside");
    assert_eq!(git(root, &["branch", "--list"]), branches);
    let _ = std::fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn a_non_utf8_destination_is_created_exactly_where_it_was_checked() {
    use std::os::unix::ffi::OsStringExt;

    let (fixture, base) = moved_on();
    let root = fixture.root();
    let dir = PathBuf::from(std::ffi::OsString::from_vec(b"wt-\xff-dir".to_vec()));

    let path = create_run_worktree(root, &dir, "fix--bytes", &base).unwrap();

    assert_eq!(path, root.join(&dir));
    assert!(path.join(".git").exists(), "the worktree is at the returned path");
    assert_eq!(git(&path, &["symbolic-ref", "--short", "HEAD"]), "fix--bytes");
    // The same name is now taken, and is detected as such.
    assert!(matches!(
        create_run_worktree(root, &dir, "fix--bytes2", &base),
        Err(WorktreeError::PathExists { .. })
    ));
}

#[test]
fn a_failed_worktree_add_leaves_no_branch_or_registration_so_a_retry_succeeds() {
    let (fixture, base) = moved_on();
    let root = fixture.root();
    // A regular file where a parent directory is needed: `git worktree add` fails.
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::write(root.join(".claude/worktrees"), "not a directory").unwrap();
    let branches = git(root, &["branch", "--list"]);
    let registered = git(root, &["worktree", "list", "--porcelain"]);

    let err = create_run_worktree(root, Path::new(".claude/worktrees/fix--f"), "fix--f", &base).unwrap_err();

    assert!(matches!(err, WorktreeError::Git { .. }), "{err:?}");
    assert_eq!(git(root, &["branch", "--list"]), branches, "no branch left behind");
    assert_eq!(git(root, &["worktree", "list", "--porcelain"]), registered);

    // Once the obstacle is gone the same name works.
    std::fs::remove_file(root.join(".claude/worktrees")).unwrap();
    create_run_worktree(root, Path::new(".claude/worktrees/fix--f"), "fix--f", &base).unwrap();
}

#[test]
fn concurrent_creations_of_the_same_run_leave_exactly_the_winners_branch_and_worktree() {
    use std::sync::{Arc, Barrier};

    let (fixture, base) = moved_on();
    let root = Arc::new(fixture.root().to_path_buf());
    for round in 0..25 {
        let branch = format!("fix--race-{round}");
        let dir = PathBuf::from(format!(".claude/worktrees/{branch}"));
        let gate = Arc::new(Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (root, gate, branch, dir, base) = (root.clone(), gate.clone(), branch.clone(), dir.clone(), base.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    create_run_worktree(&root, &dir, &branch, &base).is_ok()
                })
            })
            .collect();
        let wins = threads.into_iter().map(|t| t.join().unwrap() as usize).sum::<usize>();

        assert_eq!(wins, 1, "round {round}: exactly one creation wins");
        assert_eq!(git(&root, &["rev-parse", &format!("refs/heads/{branch}")]), base, "round {round}: the branch survives");
        let worktree = root.join(&dir);
        assert_eq!(git(&worktree, &["symbolic-ref", "--short", "HEAD"]), branch, "round {round}: the worktree survives");
    }
}

#[cfg(unix)]
#[test]
fn a_post_checkout_hook_failure_removes_the_populated_worktree_and_its_branch_and_a_retry_succeeds() {
    use std::os::unix::fs::PermissionsExt;

    let (fixture, base) = moved_on();
    let root = fixture.root();
    let hook = root.join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho hook-says-no >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let branches = git(root, &["branch", "--list"]);
    let registered = git(root, &["worktree", "list", "--porcelain"]);
    let dir = Path::new(".claude/worktrees/fix--hook");

    let err = create_run_worktree(root, dir, "fix--hook", &base).unwrap_err();

    assert!(matches!(&err, WorktreeError::Git { message } if message.contains("hook-says-no")), "{err:?}");
    assert!(!root.join(dir).exists(), "the populated directory is removed");
    assert_eq!(git(root, &["branch", "--list"]), branches, "the branch is removed");
    assert_eq!(git(root, &["worktree", "list", "--porcelain"]), registered, "the registration is removed");

    std::fs::remove_file(&hook).unwrap();
    create_run_worktree(root, dir, "fix--hook", &base).unwrap();
}
