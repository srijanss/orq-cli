use orq_core::FeatureStatus;
use orq_features::set_entry_status;

const LEDGER: &str = r#"[
  {
    "status": "draft",
    "featureName": "alpha",
    "extra": {
      "status": "nested \"draft\"",
      "list": [1, 2, {"status": "x"}]
    },
    "description": "first"
  },
  {
    "featureName": "beta",
    "dependsOn": ["alpha"],
    "status": "draft",
    "vendor": null
  }
]
"#;

#[test]
fn changing_one_status_changes_only_that_line() {
    let edited = set_entry_status(LEDGER, "beta", FeatureStatus::Pending).unwrap();

    let expected = LEDGER.replacen(
        "    \"status\": \"draft\",\n    \"vendor\"",
        "    \"status\": \"pending\",\n    \"vendor\"",
        1,
    );
    assert_ne!(expected, LEDGER);
    assert_eq!(edited, expected);
}

#[test]
fn the_first_entry_is_edited_without_touching_its_nested_status_fields() {
    let edited = set_entry_status(LEDGER, "alpha", FeatureStatus::InProgress).unwrap();

    let expected = LEDGER.replacen(
        "    \"status\": \"draft\",\n    \"featureName\": \"alpha\"",
        "    \"status\": \"in_progress\",\n    \"featureName\": \"alpha\"",
        1,
    );
    assert_ne!(expected, LEDGER);
    assert_eq!(edited, expected);
}

#[test]
fn an_unknown_feature_is_an_error_and_a_malformed_ledger_is_too() {
    assert!(set_entry_status(LEDGER, "gamma", FeatureStatus::Pending).is_err());
    assert!(set_entry_status("{ nope", "alpha", FeatureStatus::Pending).is_err());
}
