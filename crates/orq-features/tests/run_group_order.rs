use orq_features::{order_run_group, parse_ledger, GroupRefusal, Ineligible};

fn ledger() -> Vec<orq_features::FeatureEntry> {
    parse_ledger(
        r#"[
          {"featureName": "base", "status": "completed"},
          {"featureName": "a", "status": "pending", "dependsOn": ["b"]},
          {"featureName": "b", "status": "pending", "dependsOn": ["base"]},
          {"featureName": "c", "status": "pending"},
          {"featureName": "d", "status": "pending", "dependsOn": ["c", "a"]},
          {"featureName": "half", "status": "in_progress"},
          {"featureName": "needs-half", "status": "pending", "dependsOn": ["half"]},
          {"featureName": "needs-ghost", "status": "pending", "dependsOn": ["ghost"]},
          {"featureName": "x", "status": "pending", "dependsOn": ["y"]},
          {"featureName": "y", "status": "pending", "dependsOn": ["z"]},
          {"featureName": "z", "status": "pending", "dependsOn": ["x"]},
          {"featureName": "done", "status": "completed"}
        ]"#,
    )
    .unwrap()
}

fn names(ledger: &[orq_features::FeatureEntry], ids: &[&str]) -> Vec<String> {
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    order_run_group(ledger, &ids).unwrap().into_iter().map(|e| e.feature_name.clone()).collect()
}

#[test]
fn a_group_is_ordered_so_each_feature_follows_its_dependencies_in_the_group() {
    let ledger = ledger();
    assert_eq!(names(&ledger, &["a", "b"]), ["b", "a"]);
    assert_eq!(names(&ledger, &["d", "a", "c", "b"]), ["c", "b", "a", "d"]);
}

#[test]
fn features_with_no_order_between_them_keep_the_order_given() {
    let ledger = ledger();
    assert_eq!(names(&ledger, &["c", "b"]), ["c", "b"]);
    assert_eq!(names(&ledger, &["b", "c"]), ["b", "c"]);
}

#[test]
fn a_dependency_outside_the_group_must_already_be_completed() {
    let ledger = ledger();
    // b depends on base, which is completed: fine.
    assert_eq!(names(&ledger, &["b"]), ["b"]);
    // a's dependency b is pending and outside the group.
    let ids = vec!["a".to_string(), "c".to_string()];
    match order_run_group(&ledger, &ids) {
        Err(GroupRefusal::Feature { id, reason: Ineligible::UnmetDependencies(unmet) }) => {
            assert_eq!(id, "a");
            assert_eq!(unmet[0].name, "b");
        }
        other => panic!("{other:?}"),
    }
    let ids = vec!["needs-half".to_string()];
    assert!(matches!(
        order_run_group(&ledger, &ids),
        Err(GroupRefusal::Feature { reason: Ineligible::UnmetDependencies(_), .. })
    ));
    let ids = vec!["needs-ghost".to_string()];
    assert!(matches!(
        order_run_group(&ledger, &ids),
        Err(GroupRefusal::Feature { reason: Ineligible::UnmetDependencies(_), .. })
    ));
}

#[test]
fn a_member_that_may_not_start_is_refused_by_name() {
    let ledger = ledger();
    for (bad, expected_missing) in [("nope", true), ("done", false)] {
        let ids = vec!["c".to_string(), bad.to_string()];
        match order_run_group(&ledger, &ids) {
            Err(GroupRefusal::Feature { id, reason }) => {
                assert_eq!(id, bad);
                assert_eq!(reason == Ineligible::NotFound, expected_missing, "{reason:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_dependency_cycle_inside_the_group_is_refused_and_names_its_members() {
    let ledger = ledger();
    let ids: Vec<String> = ["c", "x", "y", "z"].iter().map(|s| s.to_string()).collect();
    match order_run_group(&ledger, &ids) {
        Err(GroupRefusal::Cycle(members)) => {
            let mut members = members;
            members.sort();
            assert_eq!(members, ["x", "y", "z"]);
        }
        other => panic!("{other:?}"),
    }
}
