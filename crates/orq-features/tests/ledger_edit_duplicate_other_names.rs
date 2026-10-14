use orq_core::FeatureStatus;
use orq_features::{set_entry_status, EditError};

#[test]
fn a_repeated_name_is_malformed_whichever_feature_is_edited() {
    let a = "{\"featureName\":\"a\",\"status\":\"draft\"}";
    let b = "{\"featureName\":\"b\",\"status\":\"draft\"}";
    let cases = [
        (format!("[{a},{b},{b}]"), "a", "duplicates after the target"),
        (format!("[{b},{b},{a}]"), "a", "duplicates before the target"),
        (format!("[{b},{b}]"), "a", "target absent"),
    ];
    for (text, name, label) in cases {
        assert_eq!(
            set_entry_status(&text, name, FeatureStatus::Pending),
            Err(EditError::Malformed),
            "{label}"
        );
    }
}
