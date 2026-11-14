//! `reopen_unfinished_features`: a run that ended without delivering, but left commits to resume,
//! puts the features its agent marked `completed` back to `in_progress`; every other entry and byte
//! of the ledger is left as it was.

use std::path::PathBuf;

use orq_features::reopen_unfinished_features;

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-reopen-unfinished-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

#[test]
fn reopen_unfinished_only_the_named_completed_features_go_back_to_in_progress() {
    let path = dir("named").join(".tdd-features.json");
    let before = "[\n  {\"featureName\": \"a\", \"status\": \"completed\", \"extra\": 1},\n  {\"featureName\": \"b\", \"status\": \"in_progress\"},\n  {\"featureName\": \"c\", \"status\": \"completed\"},\n  {\"featureName\": \"d\", \"status\": \"pending\"}\n]\n";
    std::fs::write(&path, before).unwrap();

    let reopened = reopen_unfinished_features(&path, &names(&["a", "b", "d", "missing"])).unwrap();

    assert_eq!(reopened, names(&["a"]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before.replacen("\"completed\", \"extra\"", "\"in_progress\", \"extra\"", 1));
}
