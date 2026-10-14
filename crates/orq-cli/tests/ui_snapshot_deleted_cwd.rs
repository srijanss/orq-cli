use std::process::Command;

use orq_test_support::GitFixture;

#[test]
fn a_valid_pane_directory_works_when_the_process_directory_was_deleted() {
    let repo = GitFixture::new();
    let doomed = std::env::temp_dir().join(format!("orq-doomed-ui-{}", std::process::id()));
    std::fs::create_dir_all(&doomed).unwrap();

    // Delete the process's own working directory before orq starts.
    let output = Command::new("sh")
        .arg("-c")
        .arg("cd \"$1\" && rmdir \"$1\" && exec \"$2\" ui --snapshot")
        .arg("sh")
        .arg(&doomed)
        .arg(env!("CARGO_BIN_EXE_orq"))
        .env("HERDR_ACTIVE_PANE_CWD", repo.nested_dir())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        stdout.contains(repo.root().canonicalize().unwrap().to_str().unwrap()),
        "{stdout}"
    );
}
