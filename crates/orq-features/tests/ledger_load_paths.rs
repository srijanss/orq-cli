use std::fs;
use std::path::PathBuf;

use orq_core::FeatureStatus;
use orq_features::{load_ledger, LedgerError};

fn dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("load-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_valid_ledger_loads_in_file_order_and_is_not_modified() {
    let path = dir("valid").join(".tdd-features.json");
    let text = r#"[{"featureName":"b","status":"pending"},{"featureName":"a","status":"draft"}]"#;
    fs::write(&path, text).unwrap();

    let entries = load_ledger(&path).unwrap();

    let names: Vec<_> = entries.iter().map(|e| e.feature_name.as_str()).collect();
    assert_eq!(names, ["b", "a"]);
    assert_eq!(entries[0].status.known(), Some(FeatureStatus::Pending));
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn an_empty_array_is_an_empty_ledger() {
    let path = dir("empty").join(".tdd-features.json");
    fs::write(&path, "[]").unwrap();
    assert!(load_ledger(&path).unwrap().is_empty());
}

#[test]
fn a_missing_file_is_missing_carrying_the_path_and_is_not_created() {
    let path = dir("missing").join(".tdd-features.json");

    match load_ledger(&path).unwrap_err() {
        LedgerError::Missing { path: reported } => assert_eq!(reported, path),
        other => panic!("expected Missing, got {other:?}"),
    }
    assert!(!path.exists());
}

#[test]
fn a_path_that_cannot_be_read_is_unreadable_not_missing() {
    // A directory exists but cannot be read as a file, on every platform.
    let path = dir("unreadable").join(".tdd-features.json");
    fs::create_dir(&path).unwrap();

    match load_ledger(&path).unwrap_err() {
        LedgerError::Unreadable { path: reported, .. } => assert_eq!(reported, path),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[test]
fn a_schema_error_is_malformed_without_a_position() {
    let path = dir("schema").join(".tdd-features.json");
    fs::write(&path, r#"[{"featureName":"a"}]"#).unwrap();

    match load_ledger(&path).unwrap_err() {
        LedgerError::Malformed { reason, position, .. } => {
            assert!(reason.contains("status"), "{reason}");
            assert_eq!(position, None);
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
}
