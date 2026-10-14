use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use orq_test_support::GitFixture;

fn report(body: Option<&str>) -> String {
    let repo = GitFixture::new();
    let bin = repo.root().join("herdr-stub");
    if let Some(body) = body {
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["doctor", "herdr", "--cwd"])
        .arg(repo.root())
        .env("HERDR_BIN_PATH", &bin)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_successful_version_is_reported() {
    let text = report(Some("echo 'herdr 9.9.9'"));
    assert!(text.contains("herdr version: herdr 9.9.9"), "{text}");
}

#[test]
fn a_failing_or_missing_herdr_is_unavailable_and_the_report_continues() {
    for text in [report(Some("exit 3")), report(None)] {
        assert!(text.contains("herdr version: unavailable"), "{text}");
        assert!(text.contains("repo root:"), "{text}");
    }
}

#[test]
fn control_characters_in_the_version_are_escaped() {
    let text = report(Some("printf '\\033[31mred'"));
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("red"), "{text}");
}
