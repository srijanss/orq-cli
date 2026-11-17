//! The copies of the run's logs a fix or explain session reads inside its worktree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{copy_session_files, finish_session_files, remove_session_files, SESSION_FILES_DIR, UNRELATED_FAILURE_FILE};
use orq_test_support::GitFixture;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A folder of run logs outside the repository.
fn logs(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-session-files-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_named_files_are_copied_into_the_worktree_replacing_an_earlier_copy_and_git_does_not_see_them() {
    let repo = GitFixture::new();
    let logs = logs("copy");
    fs::write(logs.join("gate.log"), "first gate\n").unwrap();
    copy_session_files(repo.root(), &[&logs.join("gate.log")]).unwrap();
    fs::write(repo.root().join(SESSION_FILES_DIR).join("left-over.txt"), "x").unwrap();
    fs::write(logs.join("gate.log"), "second gate\n").unwrap();

    let dir = copy_session_files(repo.root(), &[&logs.join("gate.log")]).unwrap();

    assert_eq!(dir, repo.root().join(".claude/orq-run"));
    assert_eq!(fs::read_to_string(dir.join("gate.log")).unwrap(), "second gate\n");
    assert!(!dir.join("left-over.txt").exists(), "an earlier copy is replaced");
    assert_eq!(git(repo.root(), &["status", "--porcelain"]), "");
}

#[test]
fn once_the_session_ends_its_report_is_kept_in_the_log_folder_and_the_copies_are_removed() {
    let repo = GitFixture::new();
    let logs = logs("finish");
    fs::write(logs.join("gate.log"), "the gate\n").unwrap();
    let dir = copy_session_files(repo.root(), &[&logs.join("gate.log")]).unwrap();
    fs::write(dir.join(UNRELATED_FAILURE_FILE), "billing clock test is flaky\n").unwrap();

    finish_session_files(repo.root(), &logs).unwrap();

    assert_eq!(fs::read_to_string(logs.join(UNRELATED_FAILURE_FILE)).unwrap(), "billing clock test is flaky\n");
    assert!(!dir.exists(), "the copies are removed");
    assert_eq!(fs::read_to_string(logs.join("gate.log")).unwrap(), "the gate\n", "the original is unchanged");
}

#[cfg(unix)]
#[test]
fn a_claude_folder_that_links_elsewhere_gets_no_copies_and_loses_nothing() {
    let repo = GitFixture::new();
    let logs = logs("linked");
    let elsewhere = logs.join("elsewhere");
    fs::create_dir_all(elsewhere.join("orq-run")).unwrap();
    fs::write(elsewhere.join("orq-run/keep.txt"), "mine\n").unwrap();
    fs::write(elsewhere.join("orq-run").join(UNRELATED_FAILURE_FILE), "not a report\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, repo.root().join(".claude")).unwrap();
    fs::write(logs.join("gate.log"), "the gate\n").unwrap();

    assert!(copy_session_files(repo.root(), &[&logs.join("gate.log")]).is_err());
    assert!(finish_session_files(repo.root(), &logs).is_err());

    assert_eq!(fs::read_to_string(elsewhere.join("orq-run/keep.txt")).unwrap(), "mine\n");
    assert!(!elsewhere.join("orq-run/gate.log").exists());
    assert!(!logs.join(UNRELATED_FAILURE_FILE).exists(), "a report behind a link is not read");
}

#[test]
fn removing_the_copies_leaves_the_rest_of_the_claude_folder_and_needs_no_copies() {
    let repo = GitFixture::new();
    let logs = logs("remove");
    fs::write(logs.join("gate.log"), "the gate\n").unwrap();
    let dir = copy_session_files(repo.root(), &[&logs.join("gate.log")]).unwrap();
    fs::write(repo.root().join(".claude/settings.json"), "{}").unwrap();

    remove_session_files(repo.root()).unwrap();
    remove_session_files(repo.root()).unwrap();

    assert!(!dir.exists());
    assert!(repo.root().join(".claude/settings.json").exists());
}
