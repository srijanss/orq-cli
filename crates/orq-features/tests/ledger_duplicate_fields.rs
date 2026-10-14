use std::fs;
use std::path::PathBuf;

use orq_features::{load_ledger, LedgerError};

#[test]
fn a_repeated_field_inside_an_entry_is_malformed() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("duplicate-fields");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    let text = r#"[{"featureName":"a","status":"pending","status":"completed"}]"#;
    fs::write(&path, text).unwrap();

    let error = load_ledger(&path).unwrap_err();

    match error {
        LedgerError::Malformed { reason, position, .. } => {
            assert!(reason.contains("duplicate field"), "{reason}");
            assert_eq!(position, None);
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}
