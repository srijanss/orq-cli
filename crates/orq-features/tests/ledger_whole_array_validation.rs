use orq_features::{validate_entries, parse_ledger, ValidationError};

fn entries(json: &str) -> Vec<orq_features::FeatureEntry> {
    parse_ledger(json).unwrap()
}

#[test]
fn a_valid_array_passes() {
    let list = entries(r#"[{"featureName":"a","status":"draft"},{"featureName":"b","status":"draft","dependsOn":["a"]}]"#);
    assert_eq!(validate_entries(&list), Ok(()));
}

#[test]
fn duplicate_names_are_reported_with_the_name() {
    let mut list = entries(r#"[{"featureName":"a","status":"draft"}]"#);
    list.push(list[0].clone());
    assert_eq!(validate_entries(&list), Err(ValidationError::DuplicateName { name: "a".into() }));
}

#[test]
fn a_missing_dependency_names_both_features() {
    let list = entries(r#"[{"featureName":"a","status":"draft","dependsOn":["ghost"]}]"#);
    assert_eq!(
        validate_entries(&list),
        Err(ValidationError::MissingDependency { feature: "a".into(), missing: "ghost".into() })
    );
}

#[test]
fn a_self_dependency_is_reported() {
    let list = entries(r#"[{"featureName":"a","status":"draft","dependsOn":["a"]}]"#);
    assert_eq!(validate_entries(&list), Err(ValidationError::SelfDependency { feature: "a".into() }));
}

#[test]
fn a_cycle_names_the_features_on_it() {
    let list = entries(
        r#"[{"featureName":"a","status":"draft","dependsOn":["c"]},{"featureName":"b","status":"draft","dependsOn":["a"]},{"featureName":"c","status":"draft","dependsOn":["b"]},{"featureName":"d","status":"draft"}]"#,
    );
    match validate_entries(&list) {
        Err(ValidationError::Cycle { features }) => {
            let mut f = features.clone();
            f.sort();
            assert_eq!(f, vec!["a", "b", "c"]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_existing_bad_entry_elsewhere_fails_the_whole_array() {
    let list = entries(
        r#"[{"featureName":"a","status":"draft"},{"featureName":"z","status":"draft","dependsOn":["nope"]},{"featureName":"b","status":"draft"}]"#,
    );
    assert!(matches!(validate_entries(&list), Err(ValidationError::MissingDependency { .. })));
}

#[test]
fn errors_display_the_features_involved() {
    let e = ValidationError::Cycle { features: vec!["a".into(), "b".into()] };
    let text = e.to_string();
    assert!(text.contains('a') && text.contains('b') && text.contains("cycle"), "{text}");
}
