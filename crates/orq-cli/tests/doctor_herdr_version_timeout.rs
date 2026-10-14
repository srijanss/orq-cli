use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orq_test_support::GitFixture;

/// Runs `orq doctor herdr` against a stub herdr with `body` as its script and
/// kills it if it has not finished within `deadline`, so a regression fails
/// the test instead of hanging the suite. Returns (stdout, elapsed).
fn doctor_with_stub(body: &str, deadline: Duration) -> (String, Duration) {
    let repo = GitFixture::new();
    let bin = repo.root().join("herdr-stub");
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
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
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("orq doctor herdr did not finish within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (reader.join().unwrap(), started.elapsed())
}

#[test]
fn a_stalled_herdr_does_not_block_the_rest_of_the_report() {
    let (text, elapsed) = doctor_with_stub("exec sleep 60", Duration::from_secs(20));

    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    assert!(text.contains("herdr version: unavailable"), "{text}");
    assert!(text.contains("repo root:"), "{text}");
}

#[test]
fn a_herdr_that_closes_stdout_and_keeps_running_is_also_cut_off() {
    let (text, elapsed) = doctor_with_stub("exec 1>&-\nexec sleep 60", Duration::from_secs(20));

    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    assert!(text.contains("herdr version: unavailable"), "{text}");
    assert!(text.contains("repo root:"), "{text}");
}
