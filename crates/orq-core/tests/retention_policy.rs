use orq_core::retention::RetentionPolicy;

#[test]
fn the_defaults_are_the_specs_suggested_policy() {
    let p = RetentionPolicy::default();
    assert_eq!((p.merged_days, p.closed_days, p.failed_days, p.logs_days), (90, 90, 30, 14));
    assert_eq!((p.temporary_days, p.checkpoint_days, p.metadata_days), (7, 30, 180));
}

#[test]
fn a_missing_retention_section_means_the_defaults() {
    assert_eq!(RetentionPolicy::parse("[other]\nx = 1\n").unwrap(), RetentionPolicy::default());
    assert_eq!(RetentionPolicy::parse("").unwrap(), RetentionPolicy::default());
}

#[test]
fn listed_keys_override_and_the_rest_keep_their_defaults() {
    let p = RetentionPolicy::parse("[retention]\nmerged_days = 10\nlogs_days = 1\n").unwrap();
    assert_eq!((p.merged_days, p.logs_days, p.failed_days), (10, 1, 30));
}

#[test]
fn bad_values_are_refused_naming_the_key() {
    for bad in ["merged_days = -1", "merged_days = \"ninety\"", "merged_days = 1.5", "merged_days = 99999999999", "surprise_days = 3"] {
        let err = RetentionPolicy::parse(&format!("[retention]\n{bad}\n")).unwrap_err();
        assert!(err.contains(bad.split(' ').next().unwrap()), "{bad}: {err}");
    }
    assert!(RetentionPolicy::parse("[retention\n").is_err());
}

#[test]
fn protection_of_active_and_unresolved_work_cannot_be_switched_off() {
    assert!(RetentionPolicy::parse("[retention]\nprotect_active = true\nprotect_unresolved_reviews = true\n").is_ok());
    let err = RetentionPolicy::parse("[retention]\nprotect_active = false\n").unwrap_err();
    assert!(err.contains("protect_active"), "{err}");
    assert!(RetentionPolicy::parse("[retention]\nprotect_unresolved_reviews = false\n").is_err());
}
