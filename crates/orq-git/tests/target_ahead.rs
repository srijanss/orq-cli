//! How many commits the local target branch has that its remote-tracking branch does not: a run branches
//! from the remote's target, so those commits are not in it.

use std::path::Path;
use std::process::Command;

use orq_git::local_commits_not_on_remote;
use orq_test_support::GitFixture;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn counts_the_local_commits_the_remote_tracking_branch_lacks() {
    let fixture = GitFixture::new();
    let root = fixture.root();
    git(root, &["branch", "-M", "main"]);
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert_eq!(local_commits_not_on_remote(root, "origin", "main"), Some(0));

    git(root, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "two"]);
    assert_eq!(local_commits_not_on_remote(root, "origin", "main"), Some(2));
}

/// Without both branches there is nothing to compare, and a name that could be read as an option is not asked about.
#[test]
fn says_nothing_without_both_branches_or_for_an_unsafe_name() {
    let fixture = GitFixture::new();
    let root = fixture.root();
    git(root, &["branch", "-M", "main"]);
    assert_eq!(local_commits_not_on_remote(root, "origin", "main"), None, "no remote-tracking branch");
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert_eq!(local_commits_not_on_remote(root, "origin", "release"), None, "no local branch");
    assert_eq!(local_commits_not_on_remote(root, "--all", "main"), None);
    assert_eq!(local_commits_not_on_remote(root, "origin", "..main"), None);
}
