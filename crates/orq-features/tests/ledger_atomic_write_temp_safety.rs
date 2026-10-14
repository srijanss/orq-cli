#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use orq_features::write_ledger_atomic;

fn scratch(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-atomic-safety-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_symlink_planted_at_the_old_predictable_temp_name_is_never_followed() {
    let dir = scratch("squatter");
    let path = dir.join(".tdd-features.json");
    let victim = dir.join("victim.txt");
    fs::write(&path, "[]").unwrap();
    fs::write(&victim, "precious").unwrap();
    // Squat every name this process could generate next, including the
    // leading extra dot and the per-call counter, so create_new must skip them.
    let squatted: Vec<_> = (0..500)
        .map(|n| dir.join(format!("..tdd-features.json.tmp-{}-{n}", std::process::id())))
        .collect();
    for squat in &squatted {
        symlink(&victim, squat).unwrap();
    }

    write_ledger_atomic(&path, "[1]").unwrap();

    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    assert_eq!(fs::read_to_string(&path).unwrap(), "[1]");
    assert!(squatted.iter().all(|s| fs::symlink_metadata(s).unwrap().file_type().is_symlink()));
}

#[test]
fn a_ledger_that_did_not_exist_is_created_private() {
    let dir = scratch("private");
    let path = dir.join(".tdd-features.json");

    write_ledger_atomic(&path, "[]").unwrap();

    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}
