use orq_features::{parse_ledger, validate_entries, ValidationError};

fn entries(json: &str) -> Vec<orq_features::FeatureEntry> {
    parse_ledger(json).unwrap()
}

#[test]
fn two_cycles_joined_by_a_bridge_report_only_one_cycle_without_the_bridge() {
    // a<->b is a cycle, c<->d is a cycle, and `bridge` sits between them
    // without being on either.
    let list = entries(
        r#"[
        {"featureName":"a","status":"draft","dependsOn":["b"]},
        {"featureName":"b","status":"draft","dependsOn":["a"]},
        {"featureName":"bridge","status":"draft","dependsOn":["a"]},
        {"featureName":"c","status":"draft","dependsOn":["d","bridge"]},
        {"featureName":"d","status":"draft","dependsOn":["c"]}
    ]"#,
    );

    let err = validate_entries(&list).unwrap_err();

    let ValidationError::Cycle { features } = err else { panic!("expected a cycle") };
    assert!(features == ["a", "b"] || features == ["c", "d"], "{features:?}");
}

#[test]
fn a_very_long_acyclic_chain_validates() {
    let mut json = String::from("[");
    for i in 0..20000 {
        if i > 0 {
            json.push(',');
        }
        if i == 0 {
            json.push_str(r#"{"featureName":"f0","status":"draft"}"#);
        } else {
            json.push_str(&format!(r#"{{"featureName":"f{i}","status":"draft","dependsOn":["f{}"]}}"#, i - 1));
        }
    }
    json.push(']');
    let list = entries(&json);

    assert_eq!(validate_entries(&list), Ok(()));
}
