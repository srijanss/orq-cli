#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use orq_features::write_ledger_atomic;

fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[test]
fn replaces_the_bytes_keeps_the_mode_and_leaves_no_temp_file() {
    let dir = tempfile_dir("replace");
    let path = dir.join(".tdd-features.json");
    fs::write(&path, "[]").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    write_ledger_atomic(&path, "[\n]\n").unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "[\n]\n");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(names(&dir), vec![".tdd-features.json".to_string()]);
}

#[test]
fn a_symlinked_ledger_is_written_through_to_its_target() {
    let dir = tempfile_dir("symlink");
    let target = dir.join("real.json");
    let link = dir.join(".tdd-features.json");
    fs::write(&target, "[]").unwrap();
    symlink("real.json", &link).unwrap();

    write_ledger_atomic(&link, "[1]").unwrap();

    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "[1]");
    assert_eq!(names(&dir), vec![".tdd-features.json".to_string(), "real.json".to_string()]);
}

#[test]
fn a_failed_write_leaves_the_original_and_no_temp_file() {
    let dir = tempfile_dir("failure");
    let path = dir.join(".tdd-features.json");
    fs::write(&path, "original").unwrap();
    // A directory in the way of the rename makes the final step fail.
    let blocked = dir.join("blocked.json");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("inner"), "x").unwrap();

    assert!(write_ledger_atomic(&blocked, "new").is_err());

    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(
        names(&dir),
        vec![".tdd-features.json".to_string(), "blocked.json".to_string()]
    );
}

fn tempfile_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-atomic-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}
