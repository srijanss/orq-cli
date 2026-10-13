use std::process::Command;

use orq_git::resolve_project;
use orq_test_support::GitFixture;

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn nested_path_resolves_to_repo_root_common_dir_and_project_name() {
    let fixture = GitFixture::new();
    let head_before = git_out(fixture.root(), &["rev-parse", "HEAD"]);
    let status_before = git_out(fixture.root(), &["status", "--porcelain"]);

    let project = resolve_project(fixture.nested_dir()).unwrap();

    let root = fixture.root().canonicalize().unwrap();
    assert_eq!(project.root, root);
    assert_eq!(project.git_common_dir, root.join(".git"));
    assert!(project.git_common_dir.is_absolute());
    assert_eq!(project.name, "repo");
    assert_eq!(git_out(fixture.root(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git_out(fixture.root(), &["status", "--porcelain"]), status_before);
}
