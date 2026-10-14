use std::fs;
use std::path::PathBuf;

use orq_features::{load_ledger, LedgerError};

#[test]
fn an_entry_written_as_a_positional_array_is_malformed() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("positional-entries");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    let text = r#"[["a", null, [], "pending", null, [], null, null]]"#;
    fs::write(&path, text).unwrap();

    match load_ledger(&path).unwrap_err() {
        LedgerError::Malformed { reason, position, .. } => {
            assert!(reason.contains("featureName"), "{reason}");
            assert_eq!(position, None);
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
}
