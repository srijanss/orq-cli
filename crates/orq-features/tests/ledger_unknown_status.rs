use orq_core::FeatureStatus;
use orq_features::{parse_ledger, LedgerStatus};

#[test]
fn an_unknown_status_is_kept_verbatim_and_does_not_fail_the_file() {
    let json = r#"[
  { "featureName": "a", "status": "pending" },
  { "featureName": "b", "status": "on_hold" },
  { "featureName": "c", "status": "Completed" }
]"#;

    let entries = parse_ledger(json).unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].status, LedgerStatus::Known(FeatureStatus::Pending));
    assert_eq!(entries[1].status, LedgerStatus::Unrecognised("on_hold".to_string()));
    assert_eq!(entries[1].status.known(), None);
    // Spelling is exact: a differently-cased known status is not recognised.
    assert_eq!(entries[2].status, LedgerStatus::Unrecognised("Completed".to_string()));
    assert_eq!(entries[0].status.known(), Some(FeatureStatus::Pending));
}
