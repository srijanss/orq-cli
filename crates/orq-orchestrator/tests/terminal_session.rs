use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use orq_orchestrator::run_in_terminal;

const LONG: Duration = Duration::from_secs(30);

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-terminal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn shell(dir: &Path, script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]).current_dir(dir);
    command
}

fn gone(pid: &str) -> bool {
    let pid: i32 = pid.trim().parse().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn the_program_runs_under_a_terminal_of_its_own_and_returns_when_it_exits() {
    let dir = scratch("tty");
    let script = "[ -t 0 ] && [ -t 1 ] && [ -t 2 ] && t=terminal; { echo $t; stty size; (: < /dev/tty) && echo controlling; pwd; } > out 2>&1";
    let start = Instant::now();

    run_in_terminal(shell(&dir, script), &dir.join("never"), LONG, LONG).unwrap();

    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    let out = std::fs::read_to_string(dir.join("out")).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "terminal", "{out}");
    assert_ne!(lines[1], "0 0", "a screen needs a size: {out}");
    assert_eq!(lines[2], "controlling", "{out}");
    assert_eq!(Path::new(lines[3]).canonicalize().unwrap(), dir.canonicalize().unwrap(), "{out}");
}

#[test]
fn a_program_that_keeps_going_is_closed_shortly_after_its_marker_appears() {
    let dir = scratch("marker");
    let start = Instant::now();

    run_in_terminal(shell(&dir, "echo $$ > pid; touch done; exec sleep 86400"), &dir.join("done"), LONG, Duration::from_millis(100)).unwrap();

    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert!(gone(&std::fs::read_to_string(dir.join("pid")).unwrap()));
}

#[test]
fn the_program_is_given_the_linger_to_finish_after_its_marker() {
    let dir = scratch("linger");

    run_in_terminal(shell(&dir, "touch done; sleep 0.3; touch finished; exec sleep 86400"), &dir.join("done"), LONG, Duration::from_secs(2)).unwrap();

    assert!(dir.join("finished").exists());
}

#[test]
fn a_program_that_never_marks_and_ignores_being_asked_to_stop_is_killed_with_all_it_started() {
    let dir = scratch("limit");
    let script = "trap '' TERM HUP; sleep 86400 & echo $! > helper; echo $$ > pid; while :; do sleep 1; done";
    let start = Instant::now();

    run_in_terminal(shell(&dir, script), &dir.join("never"), Duration::from_millis(500), LONG).unwrap();

    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert!(gone(&std::fs::read_to_string(dir.join("pid")).unwrap()));
    assert!(gone(&std::fs::read_to_string(dir.join("helper")).unwrap()));
}

#[test]
fn a_program_that_writes_a_lot_to_its_screen_does_not_block_on_it() {
    let dir = scratch("output");

    run_in_terminal(shell(&dir, "head -c 2000000 /dev/zero; touch finished"), &dir.join("never"), Duration::from_secs(20), LONG).unwrap();

    assert!(dir.join("finished").exists());
}

#[test]
fn a_program_that_cannot_start_is_an_error() {
    let dir = scratch("missing");
    assert!(run_in_terminal(Command::new(dir.join("no-such-program")), &dir.join("never"), LONG, LONG).is_err());
}
