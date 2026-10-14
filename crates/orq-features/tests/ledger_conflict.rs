use std::fs;
use std::path::PathBuf;

use orq_features::{LoadedLedger, SaveError};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-conflict-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.join(".tdd-features.json")
}

const ORIGINAL: &str = r#"[{"featureName":"a","status":"draft"}]"#;

#[test]
fn saving_an_unchanged_ledger_writes_the_new_text() {
    let path = scratch("unchanged");
    fs::write(&path, ORIGINAL).unwrap();
    let loaded = LoadedLedger::load(&path).unwrap();
    assert_eq!(loaded.entries().len(), 1);

    loaded.save(r#"[{"featureName":"a","status":"pending"}]"#).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), r#"[{"featureName":"a","status":"pending"}]"#);
}

#[test]
fn an_external_edit_since_the_load_is_a_conflict_and_nothing_is_written() {
    let path = scratch("edited");
    fs::write(&path, ORIGINAL).unwrap();
    let loaded = LoadedLedger::load(&path).unwrap();
    // Same length, different bytes: only a content comparison catches it.
    let external = r#"[{"featureName":"b","status":"draft"}]"#;
    fs::write(&path, &external).unwrap();

    let result = loaded.save(r#"[{"featureName":"a","status":"pending"}]"#);

    assert!(matches!(result, Err(SaveError::Conflict)), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), external);
    assert_eq!(ORIGINAL.len(), external.len());
}

#[test]
fn a_ledger_deleted_since_the_load_is_a_conflict_and_is_not_recreated() {
    let path = scratch("deleted");
    fs::write(&path, ORIGINAL).unwrap();
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::remove_file(&path).unwrap();

    let result = loaded.save("[]");

    assert!(matches!(result, Err(SaveError::Conflict)), "{result:?}");
    assert!(!path.exists());
}

#[test]
fn a_rewrite_with_identical_bytes_is_not_a_conflict() {
    let path = scratch("touched");
    fs::write(&path, ORIGINAL).unwrap();
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::write(&path, ORIGINAL).unwrap();

    assert!(loaded.save("[]").is_ok());
}

#[test]
fn a_ledger_that_cannot_be_inspected_is_an_io_error_not_a_conflict() {
    let path = scratch("uninspectable");
    fs::write(&path, ORIGINAL).unwrap();
    let loaded = LoadedLedger::load(&path).unwrap();
    // Reading a directory fails with something other than NotFound.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();

    let result = loaded.save("[]");

    assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
    assert!(path.is_dir());
}
