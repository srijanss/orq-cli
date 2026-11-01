use orq_features::{check_run_eligibility, parse_ledger, Ineligible, UnmetDependency};

fn ledger() -> Vec<orq_features::FeatureEntry> {
    parse_ledger(
        r#"[
          {"featureName": "base", "status": "completed"},
          {"featureName": "half", "status": "in_progress"},
          {"featureName": "ready", "status": "pending", "dependsOn": ["base"]},
          {"featureName": "free", "status": "pending"},
          {"featureName": "blocked", "status": "pending", "dependsOn": ["base", "half", "ghost"]},
          {"featureName": "idea", "status": "draft"},
          {"featureName": "busy", "status": "in_progress"},
          {"featureName": "done", "status": "completed"},
          {"featureName": "gone", "status": "abandoned"},
          {"featureName": "odd", "status": "on_hold"}
        ]"#,
    )
    .unwrap()
}

#[test]
fn a_pending_feature_with_completed_dependencies_may_start() {
    let ledger = ledger();
    assert_eq!(check_run_eligibility(&ledger, "ready").unwrap().feature_name, "ready");
    assert_eq!(check_run_eligibility(&ledger, "free").unwrap().feature_name, "free");
}

#[test]
fn any_status_other_than_pending_is_refused_with_that_status() {
    let ledger = ledger();
    for (name, status) in [
        ("idea", "draft"),
        ("busy", "in_progress"),
        ("done", "completed"),
        ("gone", "abandoned"),
        ("odd", "on_hold"),
    ] {
        assert_eq!(
            check_run_eligibility(&ledger, name).unwrap_err(),
            Ineligible::NotPending {
                status: status.to_string()
            },
            "{name}"
        );
    }
}

#[test]
fn unmet_dependencies_are_all_named_with_their_status() {
    let ledger = ledger();
    assert_eq!(
        check_run_eligibility(&ledger, "blocked").unwrap_err(),
        Ineligible::UnmetDependencies(vec![
            UnmetDependency {
                name: "half".to_string(),
                status: Some("in_progress".to_string()),
            },
            UnmetDependency {
                name: "ghost".to_string(),
                status: None,
            },
        ])
    );
}

#[test]
fn an_unknown_feature_is_refused() {
    assert_eq!(
        check_run_eligibility(&ledger(), "nope").unwrap_err(),
        Ineligible::NotFound
    );
}

#[test]
fn refusals_explain_themselves() {
    let ledger = ledger();
    assert_eq!(
        check_run_eligibility(&ledger, "idea").unwrap_err().to_string(),
        "feature is draft, only a pending feature can start a run"
    );
    assert_eq!(
        check_run_eligibility(&ledger, "blocked").unwrap_err().to_string(),
        "unmet dependencies: half (in_progress), ghost (not in the ledger)"
    );
    assert_eq!(
        check_run_eligibility(&ledger, "nope").unwrap_err().to_string(),
        "feature not found in the ledger"
    );
}
