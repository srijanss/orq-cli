use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

use orq_test_support::GitFixture;

#[test]
fn a_valid_explicit_cwd_ignores_a_non_utf8_pane_variable() {
    let repo = GitFixture::new();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo.nested_dir())
        .env("HERDR_ACTIVE_PANE_CWD", OsString::from_vec(b"/tmp/\xff".to_vec()))
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn a_valid_explicit_cwd_works_when_the_process_directory_was_deleted() {
    let repo = GitFixture::new();
    let doomed = std::env::temp_dir().join(format!("orq-doomed-{}", std::process::id()));
    std::fs::create_dir_all(&doomed).unwrap();

    // Delete the process's own working directory before orq starts.
    let output = Command::new("sh")
        .arg("-c")
        .arg("cd \"$1\" && rmdir \"$1\" && exec \"$2\" project inspect --cwd \"$3\"")
        .arg("sh")
        .arg(&doomed)
        .arg(env!("CARGO_BIN_EXE_orq"))
        .arg(repo.nested_dir())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}
