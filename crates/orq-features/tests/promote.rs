#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{promote_feature, promote_feature_waiting, LedgerError, PromoteError};

const LEDGER: &str = "[\n  {\"featureName\": \"a\", \"status\": \"draft\", \"extra\": 1},\n  {\"featureName\": \"b\", \"status\": \"completed\"},\n  {\"featureName\": \"c\", \"status\": \"weird\"}\n]\n";

fn scratch(label: &str, contents: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-promote-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn a_draft_becomes_pending_and_only_its_status_changes() {
    let path = scratch("ok", LEDGER);

    promote_feature(&path, "a").unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER.replacen("\"draft\"", "\"pending\"", 1));
}

#[test]
fn an_unknown_feature_is_a_distinct_error_and_the_file_is_unchanged() {
    let path = scratch("unknown", LEDGER);
    let result = promote_feature(&path, "nope");
    assert!(matches!(result, Err(PromoteError::UnknownFeature(ref n)) if n == "nope"), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER);
}

#[test]
fn a_non_draft_is_an_illegal_transition_and_the_file_is_unchanged() {
    let path = scratch("illegal", LEDGER);
    let result = promote_feature(&path, "b");
    assert!(matches!(result, Err(PromoteError::IllegalTransition(_))), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER);
}

#[test]
fn an_unrecognised_status_is_refused_and_the_file_is_unchanged() {
    let path = scratch("unrecognised", LEDGER);
    let result = promote_feature(&path, "c");
    assert!(matches!(result, Err(PromoteError::UnrecognisedStatus(ref s)) if s == "weird"), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER);
}

#[test]
fn a_malformed_ledger_is_reported_and_left_alone() {
    let path = scratch("malformed", "{ not json");
    let result = promote_feature(&path, "a");
    assert!(matches!(result, Err(PromoteError::Ledger(LedgerError::Malformed { .. }))), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn a_held_lock_is_a_locked_error_and_the_file_is_unchanged() {
    let path = scratch("locked", LEDGER);
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path.with_file_name(".tdd-features.json.lock"))
        .unwrap();
    lock.lock().unwrap();

    let result = promote_feature_waiting(&path, "a", Some(Duration::from_millis(50)));

    assert!(matches!(result, Err(PromoteError::Locked)), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER);
}

#[test]
fn a_lock_path_that_cannot_be_opened_is_an_io_error_and_the_file_is_unchanged() {
    let path = scratch("lock-io", LEDGER);
    fs::create_dir(path.with_file_name(".tdd-features.json.lock")).unwrap();

    let result = promote_feature_waiting(&path, "a", Some(Duration::from_millis(50)));

    assert!(matches!(result, Err(PromoteError::Io(_))), "{result:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), LEDGER);
}

#[test]
fn the_committed_entry_is_returned_without_reading_the_ledger_again() {
    let path = scratch("returned", LEDGER);

    let entry = promote_feature(&path, "a").unwrap();
    // A later writer must not be able to change what this call reports.
    fs::write(&path, "[]").unwrap();

    assert_eq!(entry.feature_name, "a");
    assert_eq!(entry.status, orq_features::LedgerStatus::Known(orq_core::FeatureStatus::Pending));
}
