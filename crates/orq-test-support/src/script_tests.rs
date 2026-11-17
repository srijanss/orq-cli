use super::*;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

const ETXTBSY: i32 = 26;
const EACCES: i32 = 13;

fn exited(code: i32) -> std::io::Result<ExitStatus> {
    Ok(ExitStatus::from_raw(code << 8))
}

#[test]
fn a_probe_that_exits_zero_means_the_script_is_ready() {
    assert_eq!(probe_verdict(Path::new("/t/tool"), exited(0)), Probe::Ready);
}

#[test]
fn a_busy_script_is_tried_again() {
    let busy = Err(std::io::Error::from_raw_os_error(ETXTBSY));
    assert_eq!(probe_verdict(Path::new("/t/tool"), busy), Probe::Busy);
}

#[test]
fn a_script_that_cannot_be_started_for_another_reason_is_a_failure_naming_the_file_and_why() {
    // As on a noexec filesystem.
    let denied = Err(std::io::Error::from_raw_os_error(EACCES));
    let Probe::Failed(message) = probe_verdict(Path::new("/t/tool"), denied) else { panic!("not a failure") };
    assert!(message.contains("/t/tool"), "{message}");
    assert!(message.contains("PermissionDenied"), "{message}");
}

#[test]
fn a_probe_that_exits_non_zero_is_a_failure_naming_the_file_and_the_status() {
    // As when `#!/usr/bin/env bash` finds no bash (env exits 127).
    let Probe::Failed(message) = probe_verdict(Path::new("/t/tool"), exited(127)) else { panic!("not a failure") };
    assert!(message.contains("/t/tool"), "{message}");
    assert!(message.contains("127"), "{message}");
}
