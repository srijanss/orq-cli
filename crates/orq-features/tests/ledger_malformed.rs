use std::fs;
use std::path::PathBuf;

use orq_features::{load_ledger, LedgerError};

fn ledger_with(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("malformed-{name}"));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    fs::write(&path, bytes).unwrap();
    path
}

fn malformed(name: &str, bytes: &[u8]) -> (PathBuf, String, Option<(usize, usize)>) {
    let path = ledger_with(name, bytes);
    let error = load_ledger(&path).unwrap_err();
    // The file is never modified.
    assert_eq!(fs::read(&path).unwrap(), bytes);
    match error {
        LedgerError::Malformed { path: reported, reason, position } => {
            assert_eq!(reported, path);
            (reported, reason, position)
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
fn invalid_json_is_malformed_with_line_and_column() {
    let (_, reason, position) = malformed("syntax", b"[\n  { \"featureName\": }\n]\n");
    assert!(!reason.is_empty());
    assert_eq!(position.map(|(line, _)| line), Some(2));
}

#[test]
fn a_document_that_is_not_an_array_is_malformed() {
    let (_, reason, position) = malformed("object", br#"{"featureName": "a"}"#);
    assert!(reason.contains("array"), "{reason}");
    assert_eq!(position, None);
}

#[test]
fn an_entry_without_a_string_feature_name_is_malformed() {
    for (name, text) in [
        ("no-name", r#"[{"status": "pending"}]"#),
        ("numeric-name", r#"[{"featureName": 7, "status": "pending"}]"#),
    ] {
        let (_, reason, _) = malformed(name, text.as_bytes());
        assert!(reason.contains("featureName"), "{reason}");
    }
}

#[test]
fn a_repeated_feature_name_is_malformed_naming_it() {
    let text = r#"[{"featureName": "a", "status": "pending"}, {"featureName": "a", "status": "draft"}]"#;
    let (_, reason, _) = malformed("duplicate", text.as_bytes());
    assert!(reason.contains("\"a\"") || reason.contains("'a'"), "{reason}");
}
