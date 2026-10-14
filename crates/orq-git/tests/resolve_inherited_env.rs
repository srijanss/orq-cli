use orq_git::resolve_project;
use orq_test_support::GitFixture;

#[test]
fn inherited_git_environment_does_not_override_discovery() {
    let repo_a = GitFixture::new();
    let repo_b = GitFixture::new();
    // As when launched from a Git hook: variables that select repository A.
    std::env::set_var("GIT_DIR", repo_a.root().join(".git"));
    std::env::set_var("GIT_WORK_TREE", repo_a.root());
    std::env::set_var("GIT_COMMON_DIR", repo_a.root().join(".git"));

    let project = resolve_project(repo_b.nested_dir()).unwrap();

    assert_eq!(project.root, repo_b.root().canonicalize().unwrap());
}
