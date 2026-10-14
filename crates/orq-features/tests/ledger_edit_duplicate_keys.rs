use orq_core::FeatureStatus;
use orq_features::{set_entry_status, EditError};

#[test]
fn repeated_fields_and_repeated_names_are_malformed_like_the_loader_treats_them() {
    let cases = [
        ("[{\"featureName\":\"a\",\"status\":\"draft\",\"status\":null}]", "repeated status"),
        ("[{\"featureName\":\"a\",\"featureName\":\"b\",\"status\":\"draft\"}]", "repeated featureName"),
        (
            "[{\"featureName\":\"a\",\"status\":\"draft\"},{\"featureName\":\"a\",\"status\":\"draft\"}]",
            "repeated entry name",
        ),
    ];
    for (text, label) in cases {
        assert_eq!(
            set_entry_status(text, "a", FeatureStatus::Pending),
            Err(EditError::Malformed),
            "{label}"
        );
    }
}
