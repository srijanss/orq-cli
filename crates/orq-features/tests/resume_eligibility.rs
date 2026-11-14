//! Resuming a run accepts its features as its agent left them: `in_progress`, or `completed` when no
//! later run took the feature (`free`). A fresh run still needs `pending`; the other rules hold for both.

use orq_features::{check_resume_eligibility, order_resume_group, parse_ledger, GroupRefusal, Ineligible, UnmetDependency};

fn ledger() -> Vec<orq_features::FeatureEntry> {
    parse_ledger(
        r#"[
          {"featureName": "base", "status": "completed"},
          {"featureName": "half", "status": "in_progress"},
          {"featureName": "marked", "status": "completed"},
          {"featureName": "waiting", "status": "pending"},
          {"featureName": "stuck", "status": "in_progress", "dependsOn": ["waiting"]},
          {"featureName": "idea", "status": "draft"},
          {"featureName": "gone", "status": "abandoned"}
        ]"#,
    )
    .unwrap()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| n.to_string()).collect()
}

#[test]
fn a_resume_accepts_a_pending_an_in_progress_and_a_free_completed_feature() {
    let ledger = ledger();
    let free = names(&["marked"]);
    for name in ["waiting", "half", "marked"] {
        assert_eq!(check_resume_eligibility(&ledger, name, &free).unwrap().feature_name, name);
    }
}

#[test]
fn a_resume_refuses_a_completed_feature_a_later_run_took() {
    let ledger = ledger();

    let refused = check_resume_eligibility(&ledger, "marked", &[]).unwrap_err();

    assert_eq!(refused, Ineligible::TakenByLaterRun);
    assert!(refused.to_string().contains("a later run"), "{refused}");
}

#[test]
fn a_resume_refuses_any_other_status_and_unmet_dependencies() {
    let ledger = ledger();
    let free = names(&["marked"]);
    for (name, status) in [("idea", "draft"), ("gone", "abandoned")] {
        assert_eq!(check_resume_eligibility(&ledger, name, &free), Err(Ineligible::NotResumable { status: status.to_string() }));
    }
    assert_eq!(
        check_resume_eligibility(&ledger, "stuck", &free),
        Err(Ineligible::UnmetDependencies(vec![UnmetDependency { name: "waiting".to_string(), status: Some("pending".to_string()) }]))
    );
    assert_eq!(check_resume_eligibility(&ledger, "ghost", &free), Err(Ineligible::NotFound));
}

#[test]
fn a_group_resume_takes_its_members_as_their_agents_left_them() {
    let ledger = ledger();
    let ids = names(&["stuck", "waiting", "marked"]);

    let ordered: Vec<String> = order_resume_group(&ledger, &ids, &names(&["marked"])).unwrap().iter().map(|e| e.feature_name.clone()).collect();

    assert_eq!(ordered, names(&["waiting", "stuck", "marked"]));
    assert_eq!(
        order_resume_group(&ledger, &ids, &[]),
        Err(GroupRefusal::Feature { id: "marked".to_string(), reason: Ineligible::TakenByLaterRun })
    );
}
