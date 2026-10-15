use orq_features::{AppendError, FeatureDraft, LoadedLedger, SaveError};

#[test]
fn an_explicit_absent_snapshot_conflicts_with_a_file_that_appeared_since() {
    let dir = std::env::temp_dir().join(format!("orq-absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    // The caller observed the file as missing, then another process wrote it.
    let snapshot = LoadedLedger::absent(&path);
    std::fs::write(&path, r#"[{"featureName":"other","status":"draft"}]"#).unwrap();

    let result = snapshot.append(&FeatureDraft::new("mine"));

    assert!(snapshot.is_new());
    assert!(matches!(result, Err(AppendError::Save(SaveError::Conflict))), "{result:?}");
    assert!(std::fs::read_to_string(&path).unwrap().contains("other"));
}
