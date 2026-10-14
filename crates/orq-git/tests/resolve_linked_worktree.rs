use orq_git::resolve_project;
use orq_test_support::GitFixture;

#[test]
fn linked_worktree_shares_the_project_identity_but_keeps_its_own_root() {
    let fixture = GitFixture::new();
    let worktree = fixture.add_linked_worktree("feature");

    let main = resolve_project(fixture.root()).unwrap();
    let linked = resolve_project(&worktree).unwrap();

    assert!(!main.is_linked_worktree);
    assert!(linked.is_linked_worktree);
    assert_eq!(linked.git_common_dir, main.git_common_dir);
    assert_eq!(linked.name, main.name);
    assert_eq!(linked.root, worktree.canonicalize().unwrap());
    assert_ne!(linked.root, main.root);
}
