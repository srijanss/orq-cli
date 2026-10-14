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
    assert_eq!(entries[0].status.known(), Some(FeatureStatus::Completed));
    assert_eq!(entries[0].test_file.as_deref(), Some("-p orq-core --test first"));
    assert_eq!(entries[0].target_files, vec!["crates/orq-core/src/lib.rs".to_string()]);
    assert_eq!(entries[0].recorded_at.as_deref(), Some("2026-10-14T01:00:00Z"));
    assert_eq!(entries[0].cycles_completed, Some(3));
    assert_eq!(entries[1].feature_name, "second");
    assert_eq!(entries[1].description, None);
    assert_eq!(entries[1].status.known(), Some(FeatureStatus::Draft));
}

#[test]
fn malformed_or_mistyped_ledgers_are_rejected_as_a_whole() {
    let valid = r#"{"featureName":"ok","status":"draft"}"#;
    let cases = [
        ("not json", "{".to_string()),
        ("root is an object", r#"{"featureName":"a","status":"draft"}"#.to_string()),
        ("root is a string", r#""[]""#.to_string()),
        ("entry is not an object", "[1]".to_string()),
        ("entry is a positional array", r#"[["a","draft"]]"#.to_string()),
        ("missing featureName", r#"[{"status":"draft"}]"#.to_string()),
        ("missing status", r#"[{"featureName":"a"}]"#.to_string()),
        ("status is not a string", r#"[{"featureName":"a","status":1}]"#.to_string()),
        ("dependsOn is not a list", r#"[{"featureName":"a","status":"draft","dependsOn":"b"}]"#.to_string()),
        ("cyclesCompleted is negative", r#"[{"featureName":"a","status":"draft","cyclesCompleted":-1}]"#.to_string()),
        ("one bad entry rejects the valid one", format!("[{valid},{{\"status\":\"draft\"}}]")),
    ];
    for (label, text) in cases {
        assert!(parse_ledger(&text).is_err(), "{label}");
    }
}

#[test]
fn an_empty_ledger_and_a_minimal_entry_use_the_documented_defaults() {
    assert!(parse_ledger("[]").unwrap().is_empty());

    let entries = parse_ledger(r#"[{"featureName":"min","status":"in_progress"}]"#).unwrap();

    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.feature_name, "min");
    assert_eq!(entry.status.known(), Some(FeatureStatus::InProgress));
    assert_eq!(entry.description, None);
    assert!(entry.depends_on.is_empty());
    assert_eq!(entry.test_file, None);
    assert!(entry.target_files.is_empty());
    assert_eq!(entry.recorded_at, None);
    assert_eq!(entry.cycles_completed, None);
}
