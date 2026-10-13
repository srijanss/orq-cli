use std::process::Command;

#[test]
fn orq_version_prints_version_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .arg("--version")
        .output()
        .expect("failed to run orq");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "stdout: {stdout}");
}
