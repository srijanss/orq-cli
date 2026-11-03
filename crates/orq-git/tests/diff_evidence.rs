use std::path::Path;
use std::process::Command;

use orq_git::{diff_evidence, DiffError};
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

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

#[test]
fn a_change_reports_resolved_shas_and_line_counts() {
    let fixture = GitFixture::new();
    let root = fixture.root();
    let base = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(root.join("README.md"), "changed\n").unwrap();
    let head = commit_all(root, "change");

    let evidence = diff_evidence(root, "HEAD~1", "HEAD").unwrap();
    assert_eq!(evidence.base_sha, base);
    assert_eq!(evidence.head_sha, head);
    assert_eq!(evidence.files.len(), 2);
    let a = evidence.files.iter().find(|f| f.path == "a.txt").unwrap();
    assert_eq!((a.added, a.deleted, a.old_path.clone()), (Some(3), Some(0), None));
    let readme = evidence.files.iter().find(|f| f.path == "README.md").unwrap();
    assert_eq!((readme.added, readme.deleted), (Some(1), Some(1)));
}

#[test]
fn no_changed_files_is_an_empty_list_not_an_error() {
    let fixture = GitFixture::new();
    let evidence = diff_evidence(fixture.root(), "HEAD", "HEAD").unwrap();
    assert!(evidence.files.is_empty());
    assert_eq!(evidence.base_sha, evidence.head_sha);
}

#[test]
fn binary_files_have_unknown_line_counts() {
    let fixture = GitFixture::new();
    let root = fixture.root();
    std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    commit_all(root, "binary");
    let evidence = diff_evidence(root, "HEAD~1", "HEAD").unwrap();
    let blob = &evidence.files[0];
    assert_eq!(blob.path, "blob.bin");
    assert_eq!((blob.added, blob.deleted), (None, None));
}

#[test]
fn a_rename_names_both_paths() {
    let fixture = GitFixture::new();
    let root = fixture.root();
    std::fs::write(root.join("old name.txt"), "stable content line\n".repeat(10)).unwrap();
    commit_all(root, "add");
    git(root, &["mv", "old name.txt", "new\tname.txt"]);
    commit_all(root, "rename");
    let evidence = diff_evidence(root, "HEAD~1", "HEAD").unwrap();
    assert_eq!(evidence.files.len(), 1);
    assert_eq!(evidence.files[0].path, "new\tname.txt");
    assert_eq!(evidence.files[0].old_path.as_deref(), Some("old name.txt"));
}

#[test]
fn an_unknown_revision_is_an_error() {
    let fixture = GitFixture::new();
    let err = diff_evidence(fixture.root(), "no-such-ref", "HEAD").unwrap_err();
    assert!(matches!(err, DiffError::Git(_)), "{err:?}");
    let err = diff_evidence(Path::new("/nonexistent-orq-dir"), "HEAD", "HEAD").unwrap_err();
    assert!(matches!(err, DiffError::Git(_)), "{err:?}");
}
