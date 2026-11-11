//! A ledger can be taken from bytes the caller already read, so a caller that checked those bytes (say,
//! against a version a browser was shown) saves against exactly them: a file that differs is a conflict.

use orq_features::{EditFieldsError, FieldEdits, LoadedLedger, SaveError};

const LEDGER: &str = r#"[{"featureName":"a","status":"pending","description":"old"}]"#;

fn dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-from-bytes-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_save_goes_ahead_only_while_the_file_still_holds_the_bytes_given() {
    let path = dir("same").join(".tdd-features.json");
    std::fs::write(&path, LEDGER).unwrap();
    let loaded = LoadedLedger::from_bytes(&path, LEDGER.as_bytes().to_vec()).unwrap();
    assert_eq!(loaded.entries()[0].feature_name, "a");
    let edits = FieldEdits { description: Some("new".into()), ..FieldEdits::default() };
    assert_eq!(loaded.edit_fields("a", &edits).unwrap().description.as_deref(), Some("new"));

    // Bytes that are not what the file holds now: nothing is written.
    let path = dir("differs").join(".tdd-features.json");
    std::fs::write(&path, LEDGER).unwrap();
    let older = r#"[{"featureName":"a","status":"pending","description":"older"}]"#;
    let loaded = LoadedLedger::from_bytes(&path, older.as_bytes().to_vec()).unwrap();
    assert!(matches!(loaded.edit_fields("a", &edits), Err(EditFieldsError::Save(SaveError::Conflict))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), LEDGER);

    assert!(LoadedLedger::from_bytes(&path, b"not json".to_vec()).is_err());
}
