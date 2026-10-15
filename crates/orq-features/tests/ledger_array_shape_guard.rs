use orq_features::{LedgerError, LoadedLedger, SaveError};

fn dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-shape-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_missing_file_is_a_valid_empty_ledger() {
    let path = dir("missing").join(".tdd-features.json");

    let ledger = LoadedLedger::load_or_new(&path).unwrap();

    assert!(ledger.entries().is_empty());
    assert!(ledger.is_new());
    assert!(!path.exists(), "loading must not create the file");
}

#[test]
fn a_non_array_top_level_is_a_read_only_error_and_the_file_is_untouched() {
    for (label, text) in [("object", r#"{"featureName":"a"}"#), ("string", r#""x""#), ("number", "3"), ("null", "null")] {
        let path = dir(label).join(".tdd-features.json");
        std::fs::write(&path, text).unwrap();

        let error = LoadedLedger::load_or_new(&path).unwrap_err();

        match error {
            LedgerError::Malformed { reason, .. } => assert!(reason.contains("array"), "{label}: {reason}"),
            other => panic!("{label}: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn malformed_json_is_reported_with_a_position_and_never_written() {
    let path = dir("syntax").join(".tdd-features.json");
    std::fs::write(&path, "[{\"featureName\": ").unwrap();

    let error = LoadedLedger::load_or_new(&path).unwrap_err();

    assert!(matches!(error, LedgerError::Malformed { position: Some(_), .. }), "{error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[{\"featureName\": ");
}

#[test]
fn a_new_ledger_saves_once_and_a_file_that_appeared_meanwhile_is_a_conflict() {
    let path = dir("new").join(".tdd-features.json");
    let ledger = LoadedLedger::load_or_new(&path).unwrap();
    std::fs::write(&path, "[]").unwrap();

    let result = ledger.save("[{\"featureName\":\"a\",\"status\":\"draft\"}]");

    assert!(matches!(result, Err(SaveError::Conflict)), "{result:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");

    std::fs::remove_file(&path).unwrap();
    ledger.save("[{\"featureName\":\"a\",\"status\":\"draft\"}]").unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"a\""));
}
