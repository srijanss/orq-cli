use orq_core::FeatureStatus;
use orq_features::{load_ledger, set_entry_status, LedgerError};

fn ledger(label: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-par-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn true_false_and_absent_load_as_a_tri_state() {
    let path = ledger(
        "tri",
        r#"[{"featureName":"a","status":"draft","parallelizable":true},
            {"featureName":"b","status":"draft","parallelizable":false},
            {"featureName":"c","status":"draft"}]"#,
    );

    let entries = load_ledger(&path).unwrap();

    assert_eq!(entries[0].parallelizable, Some(true));
    assert_eq!(entries[1].parallelizable, Some(false));
    assert_eq!(entries[2].parallelizable, None);
}

#[test]
fn a_non_boolean_value_is_rejected_naming_the_feature() {
    for (label, value) in [("string", r#""yes""#), ("number", "1"), ("null", "null"), ("array", "[true]")] {
        let text = format!(r#"[{{"featureName":"fast-one","status":"draft","parallelizable":{value}}}]"#);
        let path = ledger(label, &text);

        let error = load_ledger(&path).unwrap_err();

        match error {
            LedgerError::Malformed { reason, .. } => {
                assert!(reason.contains("fast-one") && reason.contains("parallelizable"), "{label}: {reason}");
            }
            other => panic!("{label}: {other:?}"),
        }
    }
}

#[test]
fn an_unrelated_edit_leaves_the_field_exactly_as_it_was() {
    let text = r#"[{"featureName":"a","status":"draft","parallelizable":false},{"featureName":"b","status":"draft"}]"#;

    let edited = set_entry_status(text, "a", FeatureStatus::Pending).unwrap();
    let other = set_entry_status(text, "b", FeatureStatus::Pending).unwrap();

    assert_eq!(edited, text.replacen("\"draft\"", "\"pending\"", 1));
    assert!(other.contains(r#""parallelizable":false"#));
    assert!(!other.split("\"featureName\":\"b\"").nth(1).unwrap().contains("parallelizable"));
}
