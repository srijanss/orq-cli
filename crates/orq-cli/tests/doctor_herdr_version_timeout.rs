use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use orq_test_support::GitFixture;

#[test]
fn a_stalled_herdr_does_not_block_the_rest_of_the_report() {
    let repo = GitFixture::new();
    let bin = repo.root().join("herdr-stub");
    std::fs::write(&bin, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = Instant::now();

    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["doctor", "herdr", "--cwd"])
        .arg(repo.root())
        .env("HERDR_BIN_PATH", &bin)
        .output()
        .unwrap();

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
    assert!(text.contains("herdr version: unavailable"), "{text}");
    assert!(text.contains("repo root:"), "{text}");
}
