//! `release_held_features`: before a group member's session, the earlier members a run holds `in_progress`
//! until delivery are set `completed`, so the member's dependencies are met; every other entry and byte of
//! the ledger is left as it was.

use std::path::PathBuf;

use orq_features::release_held_features;

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-release-held-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

#[test]
fn only_the_named_in_progress_features_are_released_completed() {
    let path = dir("named").join(".tdd-features.json");
    let before = "[\n  {\"featureName\": \"a\", \"status\": \"in_progress\", \"extra\": 1},\n  {\"featureName\": \"b\", \"status\": \"pending\"},\n  {\"featureName\": \"c\", \"status\": \"completed\"},\n  {\"featureName\": \"d\", \"status\": \"abandoned\"},\n  {\"featureName\": \"e\", \"status\": \"in_progress\"}\n]\n";
    std::fs::write(&path, before).unwrap();

    let released = release_held_features(&path, &names(&["a", "b", "c", "d", "missing"])).unwrap();

    assert_eq!(released, names(&["a"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before.replacen("\"in_progress\", \"extra\"", "\"completed\", \"extra\"", 1));
}

#[test]
fn a_missing_ledger_releases_nothing_and_is_not_created() {
    let path = dir("missing").join(".tdd-features.json");

    let released = release_held_features(&path, &names(&["a"])).unwrap();

    assert!(released.is_empty());
    assert!(!path.exists());
}
