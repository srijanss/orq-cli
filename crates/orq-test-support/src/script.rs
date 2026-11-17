use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The line put right after the shebang: a probe run exits before the body does anything.
const PROBE_LINE: &str = "[ -n \"$ORQ_TEST_EXEC_PROBE\" ] && exit 0\n";
/// The shebangs a script may start with: shells that understand the probe line.
const SHEBANGS: [&str; 3] = ["#!/bin/sh", "#!/bin/bash", "#!/usr/bin/env bash"];
/// How long a busy script is waited for, and how often it is tried meanwhile.
const BUSY_LIMIT: Duration = Duration::from_secs(2);
const BUSY_RETRY: Duration = Duration::from_millis(5);

/// Writes `body` (a shell script) to `path`, makes it executable, and returns only once it can be
/// run.
///
/// Tests in one binary run as threads. While this thread has the script open for writing, another
/// thread may fork; the child keeps a copy of the writable handle until it execs, and running the
/// script in that moment fails with ETXTBSY ("text file busy"). So the script is run with
/// `ORQ_TEST_EXEC_PROBE=1` (which makes it exit at once, without side effects) until it is no
/// longer busy.
pub fn write_executable(path: &Path, body: &str) {
    let (shebang, rest) = body.split_once('\n').unwrap_or((body, ""));
    // The probe line is shell, so only a shell may run the script.
    if !SHEBANGS.contains(&shebang.trim_end()) {
        panic!(
            "test script {}: unsupported shebang {shebang:?}; write_executable takes a body starting with one of {}",
            path.display(),
            SHEBANGS.join(", ")
        );
    }
    fs::write(path, format!("{shebang}\n{PROBE_LINE}{rest}"))
        .unwrap_or_else(|err| panic!("write test script {}: {err}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|err| panic!("make test script {} executable: {err}", path.display()));
    let started = Instant::now();
    loop {
        let probe = Command::new(path)
            .env("ORQ_TEST_EXEC_PROBE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match probe_verdict(path, probe) {
            Probe::Ready => return,
            Probe::Failed(message) => panic!("{message}"),
            Probe::Busy => {
                if started.elapsed() >= BUSY_LIMIT {
                    panic!(
                        "test script {} was still busy (ETXTBSY, text file busy) after {BUSY_LIMIT:?}: \
                         something still holds it open for writing",
                        path.display()
                    );
                }
                std::thread::sleep(BUSY_RETRY);
            }
        }
    }
}

/// What one probe run says about the script.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// It ran, and the probe line exited 0.
    Ready,
    /// ETXTBSY: something still holds it open for writing; try again.
    Busy,
    /// It cannot be run (a noexec filesystem, a missing interpreter): the message says why.
    Failed(String),
}

fn probe_verdict(path: &Path, probe: std::io::Result<ExitStatus>) -> Probe {
    match probe {
        Ok(status) if status.success() => Probe::Ready,
        Ok(status) => Probe::Failed(format!(
            "test script {} cannot be run: its probe run exited with {status}",
            path.display()
        )),
        Err(err) if err.kind() == ErrorKind::ExecutableFileBusy => Probe::Busy,
        Err(err) => Probe::Failed(format!(
            "test script {} cannot be run: {:?} ({err})",
            path.display(),
            err.kind()
        )),
    }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;
