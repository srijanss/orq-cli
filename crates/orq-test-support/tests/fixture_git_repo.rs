use std::process::Command;

use orq_test_support::GitFixture;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn plain_repo_has_a_commit_and_a_nested_subdirectory() {
    let fixture = GitFixture::new();

    assert_eq!(git(fixture.root(), &["rev-parse", "--is-inside-work-tree"]), "true");
    assert_eq!(git(fixture.root(), &["rev-list", "--count", "HEAD"]), "1");
    assert!(fixture.nested_dir().starts_with(fixture.root()));
    assert!(fixture.nested_dir().is_dir());
}

#[test]
fn dropping_the_fixture_removes_its_directory() {
    let fixture = GitFixture::new();
    let root = fixture.root().to_path_buf();
    assert!(root.exists());

    drop(fixture);

    assert!(!root.exists());
}

#[test]
fn can_add_a_linked_worktree_and_a_bare_remote_inside_the_fixture() {
    let fixture = GitFixture::new();

    let worktree = fixture.add_linked_worktree("feature");
    let bare = fixture.add_bare_remote();

    assert!(worktree.is_dir());
    assert!(!worktree.starts_with(fixture.root()));
    assert_ne!(
        git(&worktree, &["rev-parse", "--git-dir"]),
        git(&worktree, &["rev-parse", "--git-common-dir"])
    );
    assert_eq!(git(&bare, &["rev-parse", "--is-bare-repository"]), "true");
    assert!(git(fixture.root(), &["remote"]).contains("origin"));
}
