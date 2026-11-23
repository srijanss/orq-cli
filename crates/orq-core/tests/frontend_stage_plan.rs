//! A frontend run's steps: the standard ones with `design_package` after prepare and
//! `design_review` after rebase; every other run's plan is today's, unchanged.

use orq_core::stage::{Stage, StageOptions, StagePlan};

#[test]
fn a_frontend_plan_checks_the_package_before_the_implementer_and_reviews_the_design_before_the_gate() {
    let plan = StagePlan::frontend(StageOptions::default());
    let names: Vec<&str> = plan.stages().iter().map(|s| s.as_str()).collect();
    assert_eq!(
        names,
        [
            "prepare",
            "design_package",
            "implement_and_review",
            "rebase",
            "design_review",
            "verify",
            "push",
            "create_review",
            "ci_check",
            "review_ready"
        ]
    );
    for stage in plan.stages() {
        assert_eq!(Stage::parse(stage.as_str()), Some(*stage), "a stored frontend stage reads back");
    }
    assert_eq!(plan.next_after(Stage::Prepare), Some(Stage::DesignPackage));
    assert_eq!(plan.next_after(Stage::Rebase), Some(Stage::DesignReview));

    let no_rebase = StagePlan::frontend(StageOptions { rebase: false, ..StageOptions::default() });
    assert_eq!(no_rebase.next_after(Stage::ImplementAndReview), Some(Stage::DesignReview), "the design review never depends on the rebase");
}

#[test]
fn the_standard_plan_never_holds_a_design_step() {
    for bits in 0..8u8 {
        let plan = StagePlan::new(StageOptions { rebase: bits & 1 != 0, verify: bits & 2 != 0, ci_check: bits & 4 != 0 });
        assert!(!plan.stages().contains(&Stage::DesignPackage));
        assert!(!plan.stages().contains(&Stage::DesignReview));
    }
}
