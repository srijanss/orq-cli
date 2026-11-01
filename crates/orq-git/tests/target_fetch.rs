use std::path::Path;
use std::process::Command;

use orq_git::{fetch_target_base, FetchError};
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

/// A fixture whose bare origin has `main` and `release`, plus a commit on
/// `main` that exists only on the remote.
fn fixture_with_remote() -> (GitFixture, String, String) {
    let fixture = GitFixture::new();
    let remote = fixture.add_bare_remote();
    let root = fixture.root();
    git(root, &["branch", "-M", "main"]);
    git(root, &["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
    git(root, &["push", "--quiet", "origin", "HEAD:refs/heads/release"]);
    // Advance origin/main from a second clone so the local checkout is behind.
    let clone = remote.parent().unwrap().join("other");
    git(remote.parent().unwrap(), &["clone", "--quiet", "--branch", "main", remote.to_str().unwrap(), clone.to_str().unwrap()]);
    std::fs::write(clone.join("new.txt"), "new\n").unwrap();
    git(&clone, &["add", "new.txt"]);
    git(&clone, &["commit", "--quiet", "-m", "remote only"]);
    git(&clone, &["push", "--quiet", "origin", "main"]);
    let main_sha = git(&clone, &["rev-parse", "HEAD"]);
    let release_sha = git(root, &["rev-parse", "HEAD"]);
    (fixture, main_sha, release_sha)
}

#[test]
fn the_base_is_the_fetched_remote_tip_and_the_checkout_is_left_alone() {
    let (fixture, main_sha, release_sha) = fixture_with_remote();
    let root = fixture.root();
    std::fs::write(root.join("dirty.txt"), "uncommitted\n").unwrap();
    git(root, &["add", "dirty.txt"]);
    let head_before = git(root, &["rev-parse", "HEAD"]);
    let status_before = git(root, &["status", "--porcelain"]);

    let base = fetch_target_base(root, "origin", "main").unwrap();
    assert_eq!(base, main_sha);
    assert_ne!(base, head_before, "the local HEAD is not the base");
    // The exact remote-tracking commit is what was recorded.
    assert_eq!(git(root, &["rev-parse", "refs/remotes/origin/main"]), main_sha);

    // Other target branches resolve independently.
    assert_eq!(fetch_target_base(root, "origin", "release").unwrap(), release_sha);

    assert_eq!(git(root, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(root, &["status", "--porcelain"]), status_before);
}

#[test]
fn a_missing_remote_or_branch_is_a_typed_error_and_fetches_nothing() {
    let (fixture, _, _) = fixture_with_remote();
    let root = fixture.root();

    assert!(matches!(
        fetch_target_base(root, "nowhere", "main"),
        Err(FetchError::RemoteNotFound { remote }) if remote == "nowhere"
    ));
    assert!(matches!(
        fetch_target_base(root, "origin", "no-such-branch"),
        Err(FetchError::BranchNotFound { branch, .. }) if branch == "no-such-branch"
    ));
    assert!(git(root, &["for-each-ref", "refs/remotes/origin/no-such-branch"]).is_empty());
}

#[test]
fn names_that_could_be_read_as_options_or_refspecs_are_refused() {
    let (fixture, _, _) = fixture_with_remote();
    let root = fixture.root();
    for bad in ["", "-x", "--upload-pack=touch /tmp/x", "a b", "a:b", "a..b", "a\nb", "refs/heads/../x"] {
        assert!(
            matches!(fetch_target_base(root, "origin", bad), Err(FetchError::InvalidName { .. })),
            "branch {bad:?}"
        );
    }
    for bad in ["", "-o", "--exec=x", "a b", "a\nb"] {
        assert!(
            matches!(fetch_target_base(root, bad, "main"), Err(FetchError::InvalidName { .. })),
            "remote {bad:?}"
        );
    }
}
