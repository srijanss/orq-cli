use orq_core::retention::{assess, Assessment, RetentionPolicy, Retained, ReviewFacts, RunFacts};
use orq_core::review_lifecycle::ProviderState;
use orq_core::run::RunState;

const DAY: u64 = 86_400;
const NOW: u64 = 10_000 * DAY;

fn facts(state: RunState, review: Option<(ProviderState, Option<&str>)>) -> RunFacts {
    RunFacts {
        run_id: "run-1".into(),
        state,
        ended_at: Some(NOW - 5000 * DAY),
        review: review.map(|(state, error)| ReviewFacts { state, observed_at: NOW, last_error: error.map(String::from) }),
        pinned: false,
        worktree_in_use: false,
    }
}

fn retained(f: &RunFacts) -> Retained {
    match assess(&RetentionPolicy::default(), f, NOW) {
        Assessment::Retained(why) => why,
        other => panic!("expected the run to be retained, got {other:?}"),
    }
}

#[test]
fn a_failed_or_cancelled_run_with_an_open_review_is_kept() {
    for state in [RunState::Failed, RunState::Cancelled] {
        assert_eq!(retained(&facts(state, Some((ProviderState::Open, None)))), Retained::UnresolvedReview, "{state:?}");
    }
}

#[test]
fn a_failed_or_cancelled_run_whose_review_could_not_be_checked_is_kept() {
    for state in [RunState::Failed, RunState::Cancelled] {
        assert_eq!(retained(&facts(state, Some((ProviderState::Closed, Some("timeout"))))), Retained::RemoteStateUncertain, "{state:?}");
    }
}

#[test]
fn a_failed_run_with_a_closed_review_or_none_still_ages_out() {
    for review in [None, Some((ProviderState::Closed, None)), Some((ProviderState::Merged, None))] {
        assert!(matches!(assess(&RetentionPolicy::default(), &facts(RunState::Failed, review), NOW), Assessment::Prunable(_)));
    }
}
