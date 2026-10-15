use std::fs;
use std::path::PathBuf;

use orq_features::{AppendError, FeatureDraft, LoadedLedger, ValidationError};

fn ledger(label: &str, text: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-append-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    if let Some(text) = text {
        fs::write(&path, text).unwrap();
    }
    path
}

const TWO: &str = "[\n  {\"featureName\": \"a\", \"status\": \"completed\", \"extra\": {\"k\": 1}},\n  {\"featureName\":\"b\",\"status\":\"draft\"}\n]\n";

#[test]
fn append_adds_at_the_end_and_leaves_untouched_bytes_alone() {
    let path = ledger("end", Some(TWO));
    let loaded = LoadedLedger::load(&path).unwrap();
    let mut draft = FeatureDraft::new("c");
    draft.depends_on = Some(vec!["a".into()]);

    let saved = loaded.append(&draft).unwrap();

    assert_eq!(saved.feature_name, "c");
    let after = fs::read_to_string(&path).unwrap();
    let prefix = TWO.trim_end().strip_suffix(']').unwrap().trim_end();
    assert!(after.starts_with(prefix), "{after}");
    let reloaded = LoadedLedger::load(&path).unwrap();
    let names: Vec<_> = reloaded.entries().iter().map(|e| e.feature_name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert!(after.ends_with("]\n"));
}

#[test]
fn append_creates_the_array_when_the_file_is_absent() {
    let path = ledger("absent", None);
    let loaded = LoadedLedger::load_or_new(&path).unwrap();

    loaded.append(&FeatureDraft::new("first")).unwrap();

    let reloaded = LoadedLedger::load(&path).unwrap();
    assert_eq!(reloaded.entries().len(), 1);
    assert_eq!(reloaded.entries()[0].feature_name, "first");
}

#[test]
fn append_to_an_empty_array_works() {
    let path = ledger("empty", Some("[]\n"));
    LoadedLedger::load(&path).unwrap().append(&FeatureDraft::new("x")).unwrap();

    assert_eq!(LoadedLedger::load(&path).unwrap().entries().len(), 1);
}

#[test]
fn validation_failures_leave_the_file_byte_for_byte_unchanged() {
    let path = ledger("invalid", Some(TWO));
    let loaded = LoadedLedger::load(&path).unwrap();

    let mut dup = FeatureDraft::new("a");
    let err = loaded.append(&dup).unwrap_err();
    assert!(matches!(err, AppendError::Invalid(ValidationError::DuplicateName { .. })), "{err:?}");

    dup = FeatureDraft::new("c");
    dup.depends_on = Some(vec!["ghost".into()]);
    let err = loaded.append(&dup).unwrap_err();
    assert!(matches!(err, AppendError::Invalid(ValidationError::MissingDependency { .. })), "{err:?}");

    dup.depends_on = Some(vec!["c".into()]);
    let err = loaded.append(&dup).unwrap_err();
    assert!(matches!(err, AppendError::Invalid(ValidationError::SelfDependency { .. })), "{err:?}");

    assert_eq!(fs::read_to_string(&path).unwrap(), TWO);
}

#[test]
fn an_externally_changed_file_is_a_conflict() {
    let path = ledger("conflict", Some(TWO));
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::write(&path, "[]").unwrap();

    let err = loaded.append(&FeatureDraft::new("c")).unwrap_err();

    assert!(matches!(err, AppendError::Save(orq_features::SaveError::Conflict)), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
}

#[test]
fn a_blank_name_is_rejected_without_writing() {
    let path = ledger("blank", Some(TWO));
    let err = LoadedLedger::load(&path).unwrap().append(&FeatureDraft::new("  ")).unwrap_err();

    assert!(matches!(err, AppendError::Invalid(ValidationError::BlankName)), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), TWO);
}
