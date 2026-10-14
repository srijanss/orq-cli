use orq_git::resolve_project;
use orq_test_support::GitFixture;

#[test]
fn inherited_git_environment_does_not_override_discovery() {
    let repo_a = GitFixture::new();
    let repo_b = GitFixture::new();
    let linked_b = repo_b.add_linked_worktree("feature");
    // As when launched from a Git hook: variables that select repository A.
    std::env::set_var("GIT_DIR", repo_a.root().join(".git"));
    std::env::set_var("GIT_WORK_TREE", repo_a.root());
    std::env::set_var("GIT_COMMON_DIR", repo_a.root().join(".git"));

    let project = resolve_project(repo_b.nested_dir()).unwrap();
    let linked = resolve_project(&linked_b).unwrap();

    let common_b = repo_b.root().canonicalize().unwrap().join(".git");
    assert_eq!(project.root, repo_b.root().canonicalize().unwrap());
    assert_eq!(project.git_common_dir, common_b);
    assert_eq!(
        project.name,
        repo_b.root().file_name().unwrap().to_string_lossy()
    );
    assert!(!project.is_linked_worktree);
    // A linked worktree of B is still recognised as linked, of B's repository.
    assert_eq!(linked.git_common_dir, common_b);
    assert_eq!(linked.root, linked_b.canonicalize().unwrap());
    assert!(linked.is_linked_worktree);
}
