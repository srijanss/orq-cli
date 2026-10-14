use orq_core::{check_transition, FeatureStatus, IllegalTransition};

#[test]
fn pending_to_draft_is_rejected_naming_both_statuses() {
    assert_eq!(
        check_transition(FeatureStatus::Pending, FeatureStatus::Draft),
        Err(IllegalTransition {
            from: FeatureStatus::Pending,
            to: FeatureStatus::Draft,
        })
    );
}

#[test]
fn draft_to_pending_is_allowed() {
    assert_eq!(
        check_transition(FeatureStatus::Draft, FeatureStatus::Pending),
        Ok(())
    );
}

#[test]
fn every_transition_other_than_draft_to_pending_is_rejected() {
    use FeatureStatus::*;
    let all = [Draft, Pending, InProgress, Completed, Abandoned];
    for from in all {
        for to in all {
            let result = check_transition(from, to);
            if (from, to) == (Draft, Pending) {
                assert_eq!(result, Ok(()));
            } else {
                assert_eq!(result, Err(IllegalTransition { from, to }), "{from:?} -> {to:?}");
            }
        }
    }
}
