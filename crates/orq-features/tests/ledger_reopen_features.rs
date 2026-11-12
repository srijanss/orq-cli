//! `reopen_features`: features left `in_progress` by a run that produced nothing go back to
//! `pending`; every other entry and byte of the ledger is left as it was.

use std::path::PathBuf;

use orq_features::{reopen_features, reopen_features_before_save, set_entry_statuses};
use orq_core::FeatureStatus;

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-reopen-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

#[test]
fn reopen_features_only_the_named_features_that_are_in_progress_go_back_to_pending() {
    let path = dir("named").join(".tdd-features.json");
    let before = "[\n  {\"featureName\": \"a\", \"status\": \"in_progress\", \"extra\": 1},\n  {\"featureName\": \"b\", \"status\": \"completed\"},\n  {\"featureName\": \"c\", \"status\": \"in_progress\"},\n  {\"featureName\": \"d\", \"status\": \"pending\"}\n]\n";
    std::fs::write(&path, before).unwrap();

    let reopened = reopen_features(&path, &names(&["a", "b", "d", "missing"])).unwrap();

    assert_eq!(reopened, names(&["a"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before.replacen("\"in_progress\", \"extra\"", "\"pending\", \"extra\"", 1));
}

#[test]
fn reopen_features_a_ledger_with_nothing_to_reopen_is_not_written() {
    let path = dir("nothing").join(".tdd-features.json");
    let before = "[{\"featureName\":\"a\",\"status\":\"completed\"}]";
    std::fs::write(&path, before).unwrap();
    let lock = path.with_file_name(".tdd-features.json.lock");

    assert_eq!(reopen_features(&path, &names(&["a"])).unwrap(), Vec::<String>::new());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert!(!lock.exists(), "no save was attempted");
}

#[test]
fn reopen_features_a_missing_ledger_reopens_nothing_and_is_not_created() {
    let path = dir("missing").join(".tdd-features.json");

    assert_eq!(reopen_features(&path, &names(&["a"])).unwrap(), Vec::<String>::new());
    assert!(!path.exists());
}

#[test]
fn reopen_features_a_malformed_ledger_is_an_error_and_left_alone() {
    let path = dir("malformed").join(".tdd-features.json");
    std::fs::write(&path, "not json").unwrap();

    assert!(reopen_features(&path, &names(&["a"])).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
}

#[test]
fn reopen_features_several_statuses_are_set_in_one_edit_and_nothing_else_changes() {
    let before = "[\n  {\"featureName\": \"a\", \"status\": \"in_progress\"},\n  {\"status\": \"in_progress\", \"featureName\": \"b\"},\n  {\"featureName\": \"c\", \"status\": \"in_progress\"}\n]\n";

    let edited = set_entry_statuses(before, &names(&["c", "a"]), FeatureStatus::Pending).unwrap();

    assert_eq!(edited, "[\n  {\"featureName\": \"a\", \"status\": \"pending\"},\n  {\"status\": \"in_progress\", \"featureName\": \"b\"},\n  {\"featureName\": \"c\", \"status\": \"pending\"}\n]\n");
    assert_eq!(set_entry_statuses(before, &names(&["a", "missing"]), FeatureStatus::Pending), Err(orq_features::EditError::UnknownFeature));
}

#[test]
fn reopen_features_a_save_that_loses_a_race_starts_again_from_the_other_writer_s_ledger() {
    let path = dir("race").join(".tdd-features.json");
    std::fs::write(&path, "[{\"featureName\":\"a\",\"status\":\"in_progress\"},{\"featureName\":\"b\",\"status\":\"in_progress\"}]").unwrap();
    let mut raced = false;

    let reopened = reopen_features_before_save(&path, &names(&["a", "b"]), &mut || {
        if !raced {
            raced = true;
            // Another writer completes b and adds c between this load and its save.
            std::fs::write(&path, "[{\"featureName\":\"a\",\"status\":\"in_progress\"},{\"featureName\":\"b\",\"status\":\"completed\"},{\"featureName\":\"c\",\"status\":\"pending\"}]").unwrap();
        }
    })
    .unwrap();

    assert_eq!(reopened, names(&["a"]));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[{\"featureName\":\"a\",\"status\":\"pending\"},{\"featureName\":\"b\",\"status\":\"completed\"},{\"featureName\":\"c\",\"status\":\"pending\"}]"
    );
}

#[test]
fn reopen_features_a_ledger_that_keeps_changing_is_an_error_and_keeps_the_last_writer_s_text() {
    let path = dir("churn").join(".tdd-features.json");
    std::fs::write(&path, "[{\"featureName\":\"a\",\"status\":\"in_progress\"}]").unwrap();
    let mut round = 0;

    let result = reopen_features_before_save(&path, &names(&["a"]), &mut || {
        round += 1;
        std::fs::write(&path, format!("[{{\"featureName\":\"a\",\"status\":\"in_progress\",\"round\":{round}}}]")).unwrap();
    });

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("[{{\"featureName\":\"a\",\"status\":\"in_progress\",\"round\":{round}}}]"));
}
