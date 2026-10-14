use orq_core::FeatureStatus;
use orq_features::{set_entry_status, EditError};

#[test]
fn a_non_entry_after_the_target_is_malformed_just_as_before_it() {
    let before = "[42, {\"featureName\":\"a\",\"status\":\"draft\"}]";
    let after = "[{\"featureName\":\"a\",\"status\":\"draft\"}, 42]";

    assert_eq!(set_entry_status(before, "a", FeatureStatus::Pending), Err(EditError::Malformed));
    assert_eq!(set_entry_status(after, "a", FeatureStatus::Pending), Err(EditError::Malformed));
}
