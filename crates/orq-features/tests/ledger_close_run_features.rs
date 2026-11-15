//! `close_features`: the features a run's fix sessions started are closed at the run's end, `completed`
//! when the run completed and `abandoned` when it was discarded; every other entry and byte of the
//! ledger is left as it was.

use std::path::PathBuf;

use orq_core::FeatureStatus;
use orq_features::close_features;

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-close-run-features-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

const AT: &str = "2026-11-16T10:00:00Z";

#[test]
fn closing_as_completed_sets_only_the_named_in_progress_features_completed_with_when() {
    let path = dir("completed").join(".tdd-features.json");
    let before = "[\n  {\n    \"featureName\": \"a\",\n    \"status\": \"in_progress\",\n    \"extra\": 1\n  },\n  {\n    \"featureName\": \"b\",\n    \"status\": \"completed\",\n    \"recordedAt\": \"old\"\n  },\n  {\n    \"featureName\": \"c\",\n    \"status\": \"abandoned\"\n  },\n  {\n    \"featureName\": \"d\",\n    \"status\": \"pending\"\n  },\n  {\n    \"featureName\": \"e\",\n    \"status\": \"in_progress\"\n  }\n]\n";
    std::fs::write(&path, before).unwrap();

    let closed = close_features(&path, &names(&["a", "b", "c", "d", "missing"]), FeatureStatus::Completed, AT).unwrap();

    assert_eq!(closed, names(&["a"]));
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after,
        before.replacen(
            "\"status\": \"in_progress\",\n    \"extra\": 1\n",
            &format!("\"status\": \"completed\",\n    \"extra\": 1,\n    \"recordedAt\": \"{AT}\"\n"),
            1
        )
    );
}

#[test]
fn closing_as_abandoned_sets_the_named_in_progress_pending_and_completed_features_abandoned() {
    let path = dir("abandoned").join(".tdd-features.json");
    let before = "[\n  {\"featureName\": \"a\", \"status\": \"in_progress\"},\n  {\"featureName\": \"b\", \"status\": \"pending\"},\n  {\"featureName\": \"c\", \"status\": \"completed\", \"recordedAt\": \"old\"},\n  {\"featureName\": \"d\", \"status\": \"abandoned\"},\n  {\"featureName\": \"e\", \"status\": \"in_progress\"}\n]\n";
    std::fs::write(&path, before).unwrap();

    let closed = close_features(&path, &names(&["a", "b", "c", "d"]), FeatureStatus::Abandoned, AT).unwrap();

    assert_eq!(closed, names(&["a", "b", "c"]), "a feature a fix session completed goes with the discarded run's work");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before
            .replacen("\"a\", \"status\": \"in_progress\"", "\"a\", \"status\": \"abandoned\"", 1)
            .replacen("\"pending\"", "\"abandoned\"", 1)
            .replacen("\"completed\"", "\"abandoned\"", 1)
    );
}
