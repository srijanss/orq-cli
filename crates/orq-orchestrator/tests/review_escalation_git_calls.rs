//! Finding the review that escalated costs a handful of git commands however many rounds the
//! review ledger holds. Its own test binary: it puts a git that counts its calls on `PATH`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::escalated_review;
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, message: &str) -> String {
    fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn round(start: &str, head: &str, number: u32, recorded_at: &str, findings: Vec<Value>) -> Value {
    json!({
        "start": start, "head": head, "reviewer": "pi", "model": "m", "findings": findings,
        "round": number, "recordedAt": recorded_at,
    })
}

/// Held by each test: they all change the process's `PATH`.
static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Puts `PATH` back as it was when dropped, also when the test panics.
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

/// A `git` on a new `PATH` that logs each call to `calls` and runs the real one, until the
/// returned guard is dropped.
fn counting_git(root: &Path) -> (PathBuf, RestorePath) {
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("a git on PATH");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = root.join("calls");
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!("#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n", calls.display(), real.display()))
        .unwrap();
    fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    (calls, RestorePath(path))
}

/// A repository with base, feature and fix commits, and a review ledger whose round 3 reviewed
/// `base..fix` and escalated, followed by `newer` later rounds.
fn escalated_then(root: &Path, newer: impl Fn(&str, &str, &str) -> Vec<Value>) -> (PathBuf, String, String) {
    let _ = fs::remove_dir_all(root);
    let dir = root.join("repo");
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base.txt", "base");
    let first = commit(&dir, "a.txt", "feature");
    let reviewed = commit(&dir, "b.txt", "fix");
    let mut rounds = vec![round(&base, &reviewed, 3, "2026-11-07T12:00:00+00:00", vec![json!({
        "id": "leak", "severity": "high", "status": "open", "claim": "the token reaches the log",
    })])];
    rounds.extend(newer(&base, &first, &reviewed));
    fs::write(dir.join(".tdd-review-rounds.json"), json!({ format!("review:{base}"): rounds }).to_string()).unwrap();
    (dir, base, reviewed)
}

#[test]
fn the_same_out_of_order_round_repeated_is_checked_against_the_history_once() {
    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = orq_test_support::process_temp_dir().join(format!("orq-review-escalation-repeats-{}", std::process::id()));
    let (dir, base, reviewed) = escalated_then(&root, |_, first, reviewed| {
        (0..300).map(|i| round(reviewed, first, 1, &format!("2026-11-07T13:{:02}:{:02}+00:00", i / 60, i % 60), vec![])).collect()
    });
    let (calls, _path) = counting_git(&root);

    let review = escalated_review(&dir, &base).unwrap().expect("the run's review escalated");

    assert_eq!(review.reviewed_head, reviewed);
    let made = fs::read_to_string(&calls).unwrap_or_default();
    assert!(made.lines().count() <= 6, "{} git commands:\n{made}", made.lines().count());
}

#[test]
fn many_newer_rounds_that_are_not_of_this_run_cost_a_handful_of_git_commands() {
    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = orq_test_support::process_temp_dir().join(format!("orq-review-escalation-calls-{}", std::process::id()));
    let (dir, base, reviewed) = escalated_then(&root, |_, first, reviewed| {
        let mut rounds = Vec::new();
        for i in 0..300 {
            let at = format!("2026-11-07T13:{:02}:{:02}+00:00", i / 60, i % 60);
            // Reviews whose head does not come after their start, and reviews of commits this
            // repository does not have, all newer than the one that escalated.
            rounds.push(round(reviewed, first, 1, &at, vec![]));
            rounds.push(round(&format!("{i:08x}"), &format!("{:08x}", i + 1), 1, &at, vec![]));
        }
        rounds
    });
    let (calls, _path) = counting_git(&root);

    let review = escalated_review(&dir, &base).unwrap().expect("the run's review escalated");

    assert_eq!(review.reviewed_head, reviewed);
    let made = fs::read_to_string(&calls).unwrap_or_default();
    assert!(made.lines().count() <= 6, "{} git commands:\n{made}", made.lines().count());
}
