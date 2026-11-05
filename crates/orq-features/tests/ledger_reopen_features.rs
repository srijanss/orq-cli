//! `reopen_features`: features left `in_progress` by a run that produced nothing go back to
//! `pending`; every other entry and byte of the ledger is left as it was.

use std::path::PathBuf;

use orq_features::reopen_features;

fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-reopen-{}-{tag}", std::process::id()));
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
