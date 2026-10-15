use std::fs;
use std::path::PathBuf;

use orq_features::{EditFieldsError, FieldEdits, LoadedLedger, SaveError, ValidationError};

fn ledger(label: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-editf-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    fs::write(&path, text).unwrap();
    path
}

const TEXT: &str = "[\n  {\"featureName\": \"a\", \"status\": \"completed\", \"cyclesCompleted\": 2, \"future\": {\"x\": [1, 2]}},\n  {\"featureName\": \"b\", \"description\": \"old\", \"dependsOn\": [], \"status\": \"draft\", \"mystery\": true},\n  {\"featureName\":\"c\",\"status\":\"draft\"}\n]\n";

#[test]
fn changes_only_the_named_fields_and_keeps_everything_else_byte_for_byte() {
    let path = ledger("fields", TEXT);
    let loaded = LoadedLedger::load(&path).unwrap();
    let edits = FieldEdits {
        description: Some("new text".into()),
        depends_on: Some(vec!["a".into()]),
        parallelizable: Some(true),
        test_file: Some("tests/b.rs".into()),
        target_files: Some(vec!["src/b.rs".into()]),
    };

    let entry = loaded.edit_fields("b", &edits).unwrap();

    assert_eq!(entry.description.as_deref(), Some("new text"));
    assert_eq!(entry.depends_on, ["a"]);
    assert_eq!(entry.parallelizable, Some(true));
    let after = fs::read_to_string(&path).unwrap();
    // entries a and c are untouched, and b keeps its unknown and status fields
    assert!(after.contains("{\"featureName\": \"a\", \"status\": \"completed\", \"cyclesCompleted\": 2, \"future\": {\"x\": [1, 2]}}"));
    assert!(after.contains("{\"featureName\":\"c\",\"status\":\"draft\"}"));
    assert!(after.contains("\"mystery\": true"));
    assert!(after.contains("\"status\": \"draft\""));
    let reloaded = LoadedLedger::load(&path).unwrap();
    assert_eq!(reloaded.entries()[1].test_file.as_deref(), Some("tests/b.rs"));
    assert_eq!(reloaded.entries()[1].target_files, ["src/b.rs"]);
}

#[test]
fn an_empty_edit_changes_nothing() {
    let path = ledger("noop", TEXT);
    LoadedLedger::load(&path).unwrap().edit_fields("b", &FieldEdits::default()).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), TEXT);
}

#[test]
fn an_unknown_feature_is_an_error_and_nothing_is_written() {
    let path = ledger("unknown", TEXT);
    let err = LoadedLedger::load(&path)
        .unwrap()
        .edit_fields("zzz", &FieldEdits { description: Some("x".into()), ..Default::default() })
        .unwrap_err();

    assert!(matches!(err, EditFieldsError::UnknownFeature), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), TEXT);
}

#[test]
fn whole_array_validation_runs_before_saving() {
    let path = ledger("cycle", TEXT);
    let loaded = LoadedLedger::load(&path).unwrap();

    let ghost = FieldEdits { depends_on: Some(vec!["ghost".into()]), ..Default::default() };
    let err = loaded.edit_fields("b", &ghost).unwrap_err();
    assert!(matches!(err, EditFieldsError::Invalid(ValidationError::MissingDependency { .. })), "{err:?}");

    let own = FieldEdits { depends_on: Some(vec!["b".into()]), ..Default::default() };
    let err = loaded.edit_fields("b", &own).unwrap_err();
    assert!(matches!(err, EditFieldsError::Invalid(ValidationError::SelfDependency { .. })), "{err:?}");

    // c -> b, then b -> c closes a cycle
    let first = FieldEdits { depends_on: Some(vec!["b".into()]), ..Default::default() };
    loaded.edit_fields("c", &first).unwrap();
    let reloaded = LoadedLedger::load(&path).unwrap();
    let back = FieldEdits { depends_on: Some(vec!["c".into()]), ..Default::default() };
    let err = reloaded.edit_fields("b", &back).unwrap_err();
    assert!(matches!(err, EditFieldsError::Invalid(ValidationError::Cycle { .. })), "{err:?}");
}

#[test]
fn an_external_change_is_a_conflict_and_is_not_overwritten() {
    let path = ledger("conflict", TEXT);
    let loaded = LoadedLedger::load(&path).unwrap();
    fs::write(&path, "[]").unwrap();

    let err = loaded
        .edit_fields("b", &FieldEdits { description: Some("x".into()), ..Default::default() })
        .unwrap_err();

    assert!(matches!(err, EditFieldsError::Save(SaveError::Conflict)), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
}

#[test]
fn text_that_looks_like_fields_inside_strings_is_not_confused() {
    let text = "[{\"featureName\":\"a\",\"description\":\"has \\\"status\\\": and ] }\",\"status\":\"draft\"}]";
    let path = ledger("strings", text);

    LoadedLedger::load(&path)
        .unwrap()
        .edit_fields("a", &FieldEdits { test_file: Some("t.rs".into()), ..Default::default() })
        .unwrap();

    let reloaded = LoadedLedger::load(&path).unwrap();
    assert_eq!(reloaded.entries()[0].description.as_deref(), Some("has \"status\": and ] }"));
    assert_eq!(reloaded.entries()[0].test_file.as_deref(), Some("t.rs"));
}
