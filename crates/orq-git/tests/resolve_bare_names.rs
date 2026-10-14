use std::path::Path;
use std::process::Command;

use orq_git::resolve_project;
use orq_test_support::GitFixture;

fn git(args: &[&str], cwd: &Path) {
    let status = Command::new("git").current_dir(cwd).args(args).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Bare clone of the fixture at `<scratch>/<dir_name>` plus a linked worktree.
fn worktree_of_bare_clone(fixture: &GitFixture, dir_name: &str) -> std::path::PathBuf {
    let scratch = fixture.root().parent().unwrap();
    let bare = scratch.join(dir_name);
    let worktree = scratch.join(format!("wt-of-{dir_name}"));
    git(
        &["clone", "--quiet", "--bare", fixture.root().to_str().unwrap(), bare.to_str().unwrap()],
        scratch,
    );
    git(&["worktree", "add", "--quiet", worktree.to_str().unwrap()], &bare);
    worktree
}

#[test]
fn only_a_dot_git_suffix_is_stripped_from_a_bare_repository_name() {
    let fixture = GitFixture::new();

    let dotted = worktree_of_bare_clone(&fixture, "foo.bar");
    assert_eq!(resolve_project(&dotted).unwrap().name, "foo.bar");

    let suffixed = worktree_of_bare_clone(&fixture, "baz.git");
    assert_eq!(resolve_project(&suffixed).unwrap().name, "baz");
}
