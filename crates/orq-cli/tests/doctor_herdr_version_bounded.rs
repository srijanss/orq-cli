use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orq_test_support::GitFixture;

#[test]
fn a_herdr_that_floods_stdout_is_cut_off_and_reported_unavailable() {
    let repo = GitFixture::new();
    let bin = repo.root().join("herdr-stub");
    std::fs::write(&bin, "#!/bin/sh\nexec yes flood\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = Instant::now();

    let mut child = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["doctor", "herdr", "--cwd"])
        .arg(repo.root())
        .env("HERDR_BIN_PATH", &bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut pipe = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
        text
    });
    // Watchdog: a regression fails the test instead of hanging the suite.
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("orq doctor herdr did not finish within 20s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = reader.join().unwrap();

    assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
    assert!(text.contains("herdr version: unavailable"), "{text}");
    assert!(!text.contains("flood"), "{text}");
    assert!(text.len() < 4096, "{}", text.len());
}
