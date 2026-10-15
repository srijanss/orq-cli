use orq_core::FeatureStatus;
use orq_features::{parse_ledger, set_entry_status, FeatureDraft};

#[test]
fn a_draft_with_only_a_name_serialises_to_name_and_status_only() {
    let draft = FeatureDraft::new("just-a-name");

    let json = draft.to_json();

    assert_eq!(json, r#"{"featureName":"just-a-name","status":"draft"}"#);
}

#[test]
fn the_minimal_draft_is_a_valid_ledger_entry_with_nothing_filled_in() {
    let json = format!("[{}]", FeatureDraft::new("just-a-name").to_json());

    let entries = parse_ledger(&json).unwrap();

    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.status.known(), Some(FeatureStatus::Draft));
    assert_eq!(entry.description, None);
    assert!(entry.depends_on.is_empty() && entry.target_files.is_empty());
    assert_eq!((entry.test_file.as_deref(), entry.recorded_at.as_deref(), entry.cycles_completed, entry.parallelizable), (None, None, None, None));
}

#[test]
fn a_status_edit_adds_no_fields_and_keeps_unknown_properties_byte_for_byte() {
    let text = "[\n  {\"featureName\":\"a\",  \"status\":\"draft\", \"x-owner\" : {\"team\":[1,2]}}\n]\n";

    let edited = set_entry_status(text, "a", FeatureStatus::Pending).unwrap();

    assert_eq!(edited, text.replace("\"draft\"", "\"pending\""));
}

#[test]
fn only_the_fields_the_user_gave_are_written() {
    let mut draft = FeatureDraft::new("with-extras");
    draft.description = Some("why".to_string());
    draft.parallelizable = Some(false);

    let json = draft.to_json();

    assert_eq!(
        json,
        r#"{"featureName":"with-extras","description":"why","status":"draft","parallelizable":false}"#
    );
}
