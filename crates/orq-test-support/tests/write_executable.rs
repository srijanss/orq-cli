use std::fs;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use orq_test_support::{write_executable, TempDir};

#[test]
fn a_written_script_runs_at_once_and_the_probe_leaves_no_side_effect() {
    let dir = TempDir::new("orq-write-executable");
    let log = dir.join("log");
    let script = dir.join("tool");

    write_executable(&script, &format!("#!/bin/sh\necho ran >> '{}'\n", log.display()));

    assert!(!log.exists(), "the probe ran the body: {}", fs::read_to_string(&log).unwrap_or_default());
    let status = Command::new(&script).status().expect("run the written script");
    assert!(status.success());
    assert_eq!(fs::read_to_string(&log).unwrap(), "ran\n", "the body runs exactly once per call");
}

/// Runs `f` on its own thread and returns its panic message, failing if it neither panics nor
/// returns within `limit`.
fn panic_message_within(limit: Duration, f: impl FnOnce() + Send + 'static) -> Option<String> {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let message = outcome.err().map(|payload| {
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
                .unwrap_or_default()
        });
        let _ = done.send(message);
    });
    finished.recv_timeout(limit).expect("write_executable neither returned nor panicked in time")
}

#[test]
fn a_script_still_busy_after_the_limit_is_a_panic_naming_the_file_and_etxtbsy() {
    let dir = TempDir::new("orq-write-executable");
    let script = dir.join("busy-tool");
    // A writable handle that is never closed, as a forked child that never execs would hold.
    let _held = fs::OpenOptions::new().create(true).truncate(true).write(true).open(&script).unwrap();

    let path = script.clone();
    let started = Instant::now();
    let message = panic_message_within(Duration::from_secs(10), move || {
        write_executable(&path, "#!/bin/sh\nexit 0\n");
    });

    let message = message.expect("write_executable returned although the script was still busy");
    assert!(message.contains(&script.display().to_string()), "{message}");
    assert!(message.contains("ETXTBSY"), "{message}");
    assert!(started.elapsed() >= Duration::from_secs(2), "gave up before the limit: {:?}", started.elapsed());
}

#[test]
fn a_body_without_a_supported_shebang_is_a_panic_saying_which_shebangs_are_supported() {
    let dir = TempDir::new("orq-write-executable");
    for body in ["echo no shebang\n", "#!/usr/bin/env python3\nprint('hi')\n", "", "#!/bin/shell\nexit 0\n"] {
        let path = dir.join("tool");
        let message = panic_message_within(Duration::from_secs(10), move || write_executable(&path, body))
            .unwrap_or_else(|| panic!("accepted {body:?}"));
        assert!(message.contains("#!/bin/sh"), "{body:?}: {message}");
        assert!(message.contains("shebang"), "{body:?}: {message}");
    }
    for body in ["#!/bin/sh\nexit 0\n", "#!/bin/bash\nexit 0\n", "#!/usr/bin/env bash\nexit 0\n"] {
        let path = dir.join("tool");
        assert_eq!(panic_message_within(Duration::from_secs(10), move || write_executable(&path, body)), None, "{body:?}");
    }
}

#[test]
fn a_busy_script_is_waited_for_and_runs_once_the_other_handle_closes() {
    let dir = TempDir::new("orq-write-executable");
    let script = dir.join("briefly-busy-tool");
    // A second writable handle, as a forked child holds until it execs, closed after 50 ms. The
    // closer reports the moment just before it closes the handle: the script cannot be run any
    // earlier, so a correct write_executable always returns after it.
    let held = fs::OpenOptions::new().create(true).truncate(true).write(true).open(&script).unwrap();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let closing = Instant::now();
        drop(held);
        closing
    });

    write_executable(&script, "#!/bin/sh\nexit 0\n");
    let returned = Instant::now();
    let closing = closer.join().unwrap();

    assert!(returned > closing, "returned {:?} before the other handle was closed", closing - returned);
    assert!(Command::new(&script).status().expect("run the written script").success());
}

#[test]
fn scripts_written_while_other_threads_spawn_processes_never_fail_as_busy() {
    let dir = TempDir::new("orq-write-executable");
    let threads: Vec<_> = (0..16)
        .map(|index| {
            let script = dir.join(format!("tool-{index}"));
            std::thread::spawn(move || {
                write_executable(&script, "#!/bin/sh\nexit 0\n");
                for _ in 0..20 {
                    let status = Command::new(&script).status().unwrap_or_else(|err| panic!("run {script:?}: {err}"));
                    assert!(status.success());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("a script failed to run");
    }
}
