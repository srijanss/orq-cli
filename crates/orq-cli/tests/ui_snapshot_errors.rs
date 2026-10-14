use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, Output};

use orq_test_support::GitFixture;

fn orq_ui() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["ui", "--snapshot"]);
    command.env_remove("HERDR_ACTIVE_PANE_CWD");
    command
}

fn assert_failed_without_a_snapshot(output: &Output) {
    assert!(!output.status.success(), "expected failure");
    assert!(output.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(!output.stderr.is_empty(), "expected a diagnostic on stderr");
}

#[test]
fn without_a_pane_directory_the_process_directory_is_used() {
    let repo = GitFixture::new();

    let output = orq_ui().current_dir(repo.nested_dir()).output().unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let shown = String::from_utf8(output.stdout).unwrap();
    assert!(shown.contains(repo.root().canonicalize().unwrap().to_str().unwrap()), "{shown}");
}

#[test]
fn an_unavailable_git_is_an_error_not_an_empty_state() {
    let repo = GitFixture::new();
    let empty_bin = std::env::temp_dir().join(format!("orq-ui-nogit-{}", std::process::id()));
    std::fs::create_dir_all(&empty_bin).unwrap();

    let output = orq_ui()
        .env("HERDR_ACTIVE_PANE_CWD", repo.nested_dir())
        .env("PATH", &empty_bin)
        .output()
        .unwrap();

    assert_failed_without_a_snapshot(&output);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("not a Git repository"));
    let _ = std::fs::remove_dir_all(&empty_bin);
}

#[test]
fn a_non_utf8_pane_directory_is_an_error() {
    let output = orq_ui()
        .env("HERDR_ACTIVE_PANE_CWD", OsString::from_vec(b"/tmp/\xff".to_vec()))
        .output()
        .unwrap();

    assert_failed_without_a_snapshot(&output);
}

#[test]
fn a_missing_pane_directory_is_an_error_not_a_fallback() {
    let repo = GitFixture::new();

    let output = orq_ui()
        .current_dir(repo.nested_dir())
        .env("HERDR_ACTIVE_PANE_CWD", "/definitely/not/a/real/dir")
        .output()
        .unwrap();

    assert_failed_without_a_snapshot(&output);
}
