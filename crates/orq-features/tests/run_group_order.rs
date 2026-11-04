use orq_features::{order_run_group, parse_ledger, GroupRefusal, Ineligible, UnmetDependency};

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
    let unmet = |ids: &[&str]| {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        order_run_group(&ledger, &ids)
    };
    // a's dependency b is pending and outside the group.
    assert_eq!(
        unmet(&["a", "c"]),
        Err(GroupRefusal::Feature {
            id: "a".to_string(),
            reason: Ineligible::UnmetDependencies(vec![UnmetDependency {
                name: "b".to_string(),
                status: Some("pending".to_string())
            }]),
        })
    );
    assert_eq!(
        unmet(&["needs-half"]),
        Err(GroupRefusal::Feature {
            id: "needs-half".to_string(),
            reason: Ineligible::UnmetDependencies(vec![UnmetDependency {
                name: "half".to_string(),
                status: Some("in_progress".to_string())
            }]),
        })
    );
    assert_eq!(
        unmet(&["needs-ghost"]),
        Err(GroupRefusal::Feature {
            id: "needs-ghost".to_string(),
            reason: Ineligible::UnmetDependencies(vec![UnmetDependency { name: "ghost".to_string(), status: None }]),
        })
    );
}

#[test]
fn a_member_that_may_not_start_is_refused_by_name_with_its_status() {
    let ledger = ledger();
    for (bad, reason) in [
        ("nope", Ineligible::NotFound),
        ("done", Ineligible::NotPending { status: "completed".to_string() }),
        ("half", Ineligible::NotPending { status: "in_progress".to_string() }),
    ] {
        let ids = vec!["c".to_string(), bad.to_string()];
        assert_eq!(order_run_group(&ledger, &ids), Err(GroupRefusal::Feature { id: bad.to_string(), reason }));
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

#[test]
fn a_repeated_feature_is_refused_instead_of_being_run_twice() {
    let ledger = ledger();
    let ids: Vec<String> = ["c", "b", "c"].iter().map(|s| s.to_string()).collect();
    assert_eq!(order_run_group(&ledger, &ids), Err(GroupRefusal::Duplicate("c".to_string())));
}
