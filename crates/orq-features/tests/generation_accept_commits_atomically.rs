use std::fs;
use std::path::PathBuf;

use orq_features::{AppendError, FeatureDraft, LoadedLedger, SaveError};

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-accept-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn draft(name: &str, depends: &[&str]) -> FeatureDraft {
    FeatureDraft {
        feature_name: name.into(),
        description: None,
        depends_on: if depends.is_empty() { None } else { Some(depends.iter().map(|d| d.to_string()).collect()) },
        parallelizable: None,
        test_file: None,
        target_files: None,
    }
}

const EXISTING: &str = "[\n  {\"featureName\":\"base\",\"status\":\"completed\",\"cyclesCompleted\":2}\n]\n";

#[test]
fn every_accepted_candidate_is_appended_in_one_save_keeping_existing_bytes() {
    let dir = dir("many");
    let path = dir.join(".tdd-features.json");
    fs::write(&path, EXISTING).unwrap();
    let ledger = LoadedLedger::load(&path).unwrap();

    let saved = ledger.append_many(&[draft("a", &["base"]), draft("b", &["a"])]).unwrap();

    assert_eq!(saved.len(), 2);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[\n  {\"featureName\":\"base\",\"status\":\"completed\",\"cyclesCompleted\":2},\n"), "{text}");
    let entries = orq_features::load_ledger(&path).unwrap();
    assert_eq!(entries.iter().map(|e| e.feature_name.as_str()).collect::<Vec<_>>(), ["base", "a", "b"]);
}

#[test]
fn one_invalid_candidate_rejects_the_whole_batch_and_leaves_the_file_unchanged() {
    let dir = dir("invalid");
    let path = dir.join(".tdd-features.json");
    fs::write(&path, EXISTING).unwrap();
    let ledger = LoadedLedger::load(&path).unwrap();

    for batch in [
        vec![draft("ok", &[]), draft("bad", &["ghost"])],
        vec![draft("ok", &[]), draft("base", &[])],
        vec![draft("x", &["y"]), draft("y", &["x"])],
        vec![draft("same", &[]), draft("same", &[])],
    ] {
        let result = ledger.append_many(&batch);
        assert!(matches!(result, Err(AppendError::Invalid(_))), "{batch:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), EXISTING);
    }
}

#[test]
fn a_ledger_changed_since_load_is_a_conflict_and_nothing_is_written() {
    let dir = dir("conflict");
    let path = dir.join(".tdd-features.json");
    fs::write(&path, EXISTING).unwrap();
    let ledger = LoadedLedger::load(&path).unwrap();
    let changed = EXISTING.replace("completed", "pending");
    fs::write(&path, &changed).unwrap();

    let result = ledger.append_many(&[draft("a", &[])]);

    assert!(matches!(result, Err(AppendError::Save(SaveError::Conflict))));
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[test]
fn an_absent_ledger_is_created_with_all_candidates_and_a_late_file_is_a_conflict() {
    let dir = dir("absent");
    let path = dir.join(".tdd-features.json");
    let ledger = LoadedLedger::load_or_new(&path).unwrap();

    ledger.append_many(&[draft("a", &[]), draft("b", &["a"])]).unwrap();

    assert_eq!(orq_features::load_ledger(&path).unwrap().len(), 2);

    let late = dir.join("late.json");
    let snapshot = LoadedLedger::load_or_new(&late).unwrap();
    fs::write(&late, "[]").unwrap();
    assert!(matches!(snapshot.append_many(&[draft("a", &[])]), Err(AppendError::Save(SaveError::Conflict))));
    assert_eq!(fs::read_to_string(&late).unwrap(), "[]");
}

#[test]
fn a_failed_accept_keeps_the_staged_artifact_for_a_retry() {
    let dir = dir("artifact");
    let path = dir.join(".tdd-features.json");
    let artifact = dir.join("candidates-1.json");
    fs::write(&artifact, "[{\"featureName\":\"a\"}]").unwrap();
    fs::write(&path, EXISTING).unwrap();
    let ledger = LoadedLedger::load(&path).unwrap();
    fs::write(&path, "[]").unwrap();

    assert!(ledger.append_many(&[draft("a", &[])]).is_err());

    assert_eq!(fs::read_to_string(&artifact).unwrap(), "[{\"featureName\":\"a\"}]");
}
