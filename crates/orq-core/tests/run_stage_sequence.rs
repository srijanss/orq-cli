use orq_core::stage::{Stage, StageOptions, StagePlan};

#[test]
fn the_default_plan_follows_the_bash_orchestrator_order() {
    let plan = StagePlan::new(StageOptions::default());
    assert_eq!(
        plan.stages(),
        [
            Stage::Prepare,
            Stage::ImplementAndReview,
            Stage::Rebase,
            Stage::Verify,
            Stage::Push,
            Stage::CreateReview,
            Stage::CiCheck,
            Stage::ReviewReady,
        ]
    );
    assert_eq!(plan.first(), Stage::Prepare);
    let names: Vec<&str> = plan.stages().iter().map(|s| s.as_str()).collect();
    assert_eq!(
        names,
        [
            "prepare",
            "implement_and_review",
            "rebase",
            "verify",
            "push",
            "create_review",
            "ci_check",
            "review_ready"
        ]
    );
}

#[test]
fn next_stage_walks_the_plan_and_ends_after_review_ready() {
    let plan = StagePlan::new(StageOptions::default());
    let mut walked = vec![plan.first()];
    while let Some(next) = plan.next_after(*walked.last().unwrap()) {
        walked.push(next);
    }
    assert_eq!(walked, plan.stages());
    assert_eq!(plan.next_after(Stage::ReviewReady), None);
}

#[test]
fn rebase_verify_and_ci_check_can_each_be_disabled() {
    let plan = StagePlan::new(StageOptions {
        rebase: false,
        verify: false,
        ci_check: false,
    });
    assert_eq!(
        plan.stages(),
        [
            Stage::Prepare,
            Stage::ImplementAndReview,
            Stage::Push,
            Stage::CreateReview,
            Stage::ReviewReady,
        ]
    );
    assert_eq!(plan.next_after(Stage::ImplementAndReview), Some(Stage::Push));
    assert_eq!(plan.next_after(Stage::CreateReview), Some(Stage::ReviewReady));
    assert_eq!(plan.next_after(Stage::Rebase), None, "a disabled stage has no successor");

    let only_gate_off = StagePlan::new(StageOptions {
        verify: false,
        ..StageOptions::default()
    });
    assert_eq!(only_gate_off.next_after(Stage::Rebase), Some(Stage::Push));
    assert!(!only_gate_off.stages().contains(&Stage::Verify));
}

#[test]
fn no_plan_ever_contains_a_merge_stage() {
    for bits in 0..8u8 {
        let plan = StagePlan::new(StageOptions {
            rebase: bits & 1 != 0,
            verify: bits & 2 != 0,
            ci_check: bits & 4 != 0,
        });
        assert!(plan.stages().iter().all(|s| !s.as_str().contains("merge")));
        assert_eq!(plan.stages().last(), Some(&Stage::ReviewReady));
        assert_eq!(Stage::parse("merge"), None);
        for stage in plan.stages() {
            assert_eq!(Stage::parse(stage.as_str()), Some(*stage));
        }
    }
}
