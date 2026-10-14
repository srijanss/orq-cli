#![cfg(unix)]

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use orq_features::{LoadedLedger, SaveError};

const ORIGINAL: &str = r#"[{"featureName":"a","status":"draft"}]"#;
const UPDATED: &str = r#"[{"featureName":"a","status":"pending"}]"#;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-lock-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    fs::write(&path, ORIGINAL).unwrap();
    path
}

fn lock_path(ledger: &Path) -> PathBuf {
    let mut name = ledger.file_name().unwrap().to_os_string();
    name.push(".lock");
    ledger.with_file_name(name)
}

/// Takes the advisory lock the way the TDD MCP does: an exclusive flock on the
/// sibling `.lock` file.
fn hold(ledger: &Path) -> File {
    let file = OpenOptions::new().create(true).write(true).open(lock_path(ledger)).unwrap();
    file.lock().unwrap();
    file
}

fn is_free(ledger: &Path) -> bool {
    let file = OpenOptions::new().create(true).write(true).open(lock_path(ledger)).unwrap();
    match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => panic!("{e}"),
    }
}

#[test]
fn a_held_lock_gives_a_locked_error_after_the_wait_and_nothing_is_written() {
    let path = scratch("held");
    let loaded = LoadedLedger::load(&path).unwrap();
    let _held = hold(&path);

    let started = std::time::Instant::now();
    let result = loaded.save_waiting(UPDATED, Duration::from_millis(200));

    assert!(matches!(result, Err(SaveError::Locked)), "{result:?}");
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
}

#[test]
fn a_lock_released_during_the_wait_lets_the_save_go_through() {
    let path = scratch("released");
    let loaded = LoadedLedger::load(&path).unwrap();
    let held = hold(&path);
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        drop(held);
    });

    loaded.save_waiting(UPDATED, Duration::from_secs(5)).unwrap();

    releaser.join().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), UPDATED);
}

#[test]
fn an_edit_made_while_the_save_waits_for_the_lock_is_still_a_conflict() {
    let path = scratch("edit-while-waiting");
    let loaded = LoadedLedger::load(&path).unwrap();
    let held = hold(&path);
    let external = r#"[{"featureName":"b","status":"draft"}]"#;
    let editor = {
        let path = path.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            // The lock holder edits the ledger, then lets go.
            fs::write(&path, external).unwrap();
            drop(held);
        })
    };

    let result = loaded.save_waiting(UPDATED, Duration::from_secs(5));

    editor.join().unwrap();
    assert!(matches!(result, Err(SaveError::Conflict)), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
}

#[test]
fn the_lock_is_released_after_success_conflict_and_io_failure() {
    // Success.
    let path = scratch("release-ok");
    LoadedLedger::load(&path).unwrap().save(UPDATED).unwrap();
    assert!(is_free(&path), "after success");

    // Conflict.
    let path = scratch("release-conflict");
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::write(&path, "[]").unwrap();
    assert!(matches!(loaded.save(UPDATED), Err(SaveError::Conflict)));
    assert!(is_free(&path), "after conflict");

    // Io failure: the ledger became a directory, so it cannot be read.
    let path = scratch("release-io");
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(loaded.save(UPDATED), Err(SaveError::Io(_))));
    assert!(is_free(&path), "after io failure");
}

#[test]
fn the_wait_is_never_exceeded_even_if_the_lock_frees_just_after_it() {
    let path = scratch("deadline");
    let loaded = LoadedLedger::load(&path).unwrap();
    let held = hold(&path);
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        drop(held);
    });

    let result = loaded.save_waiting(UPDATED, Duration::from_millis(1));

    releaser.join().unwrap();
    assert!(matches!(result, Err(SaveError::Locked)), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
}

#[test]
fn a_fifo_in_the_lock_path_is_an_io_error_not_a_hang() {
    let path = scratch("fifo");
    let loaded = LoadedLedger::load(&path).unwrap();
    let status = std::process::Command::new("mkfifo").arg(lock_path(&path)).status().unwrap();
    assert!(status.success());

    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(loaded.save_waiting(UPDATED, Duration::from_millis(100)));
    });
    let result = rx.recv_timeout(Duration::from_secs(3)).expect("save hung on a FIFO lock path");

    assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
}
