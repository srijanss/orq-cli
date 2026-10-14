use orq_core::FeatureStatus;
use orq_features::parse_ledger;

#[test]
fn parses_every_field_of_the_real_ledger_shape_in_order() {
    let json = r#"[
  {
    "featureName": "first",
    "description": "does a thing",
    "dependsOn": ["zeroth"],
    "status": "completed",
    "testFile": "-p orq-core --test first",
    "targetFiles": ["crates/orq-core/src/lib.rs"],
    "recordedAt": "2026-10-14T01:00:00Z",
    "cyclesCompleted": 3
  },
  { "featureName": "second", "dependsOn": [], "status": "draft" }
]
"#;

    let entries = parse_ledger(json).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].feature_name, "first");
    assert_eq!(entries[0].description.as_deref(), Some("does a thing"));
    assert_eq!(entries[0].depends_on, vec!["zeroth".to_string()]);
    assert_eq!(entries[0].status, FeatureStatus::Completed);
    assert_eq!(entries[0].test_file.as_deref(), Some("-p orq-core --test first"));
    assert_eq!(entries[0].target_files, vec!["crates/orq-core/src/lib.rs".to_string()]);
    assert_eq!(entries[0].recorded_at.as_deref(), Some("2026-10-14T01:00:00Z"));
    assert_eq!(entries[0].cycles_completed, Some(3));
    assert_eq!(entries[1].feature_name, "second");
    assert_eq!(entries[1].description, None);
    assert_eq!(entries[1].status, FeatureStatus::Draft);
}
