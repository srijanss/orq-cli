use orq_core::retention::{assess, ArtifactClass, Assessment, Retained, ReviewFacts, RetentionPolicy, RunFacts};
use orq_core::review_lifecycle::ProviderState;
use orq_core::run::RunState;

const DAY: u64 = 86_400;
const NOW: u64 = 10_000 * DAY;

fn facts(state: RunState, ended_days_ago: u64, review: Option<(ProviderState, u64, Option<&str>)>) -> RunFacts {
    RunFacts {
        run_id: "run-1".into(),
        state,
        ended_at: Some(NOW - ended_days_ago * DAY),
        review: review.map(|(state, checked_secs_ago, error)| ReviewFacts { state, observed_at: NOW - checked_secs_ago, last_error: error.map(String::from) }),
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

fn prunable(f: &RunFacts) -> Vec<ArtifactClass> {
    match assess(&RetentionPolicy::default(), f, NOW) {
        Assessment::Prunable(classes) => classes,
        other => panic!("expected the run to be prunable, got {other:?}"),
    }
}

#[test]
fn queued_preparing_running_and_blocked_runs_are_never_pruned_however_old() {
    for state in [RunState::Queued, RunState::Preparing, RunState::Running, RunState::Blocked] {
        assert_eq!(retained(&facts(state, 5000, None)), Retained::Active, "{state:?}");
    }
}

#[test]
fn a_run_awaiting_human_review_is_never_pruned() {
    assert_eq!(retained(&facts(RunState::ReviewReady, 5000, Some((ProviderState::Merged, 0, None)))), Retained::AwaitingReview);
}

#[test]
fn a_pinned_run_is_never_pruned_and_pinning_outranks_everything() {
    let mut f = facts(RunState::Failed, 5000, None);
    f.pinned = true;
    assert_eq!(retained(&f), Retained::Pinned);
}

#[test]
fn a_run_whose_worktree_is_in_use_is_kept() {
    let mut f = facts(RunState::Failed, 5000, None);
    f.worktree_in_use = true;
    assert_eq!(retained(&f), Retained::WorktreeInUse);
}

#[test]
fn an_unresolved_review_is_kept_even_when_the_run_completed_long_ago() {
    assert_eq!(retained(&facts(RunState::Completed, 5000, Some((ProviderState::Open, 0, None)))), Retained::UnresolvedReview);
}

#[test]
fn when_the_provider_could_not_be_asked_the_run_is_kept() {
    let f = facts(RunState::Completed, 5000, Some((ProviderState::Merged, 0, Some("timeout"))));
    assert_eq!(retained(&f), Retained::RemoteStateUncertain);
}

#[test]
fn a_stale_closed_observation_is_not_trusted_but_a_merge_is_final() {
    let old = 3 * DAY;
    assert_eq!(retained(&facts(RunState::Completed, 5000, Some((ProviderState::Closed, old, None)))), Retained::RemoteStateUncertain);
    assert!(!prunable(&facts(RunState::Completed, 5000, Some((ProviderState::Merged, old, None)))).is_empty());
    assert!(!prunable(&facts(RunState::Completed, 5000, Some((ProviderState::Closed, 60, None)))).is_empty());
}

#[test]
fn a_completed_run_with_no_recorded_review_state_is_kept() {
    assert_eq!(retained(&facts(RunState::Completed, 5000, None)), Retained::RemoteStateUncertain);
}

#[test]
fn a_run_with_no_end_time_is_kept() {
    let mut f = facts(RunState::Failed, 5000, None);
    f.ended_at = None;
    assert_eq!(retained(&f), Retained::NoEndTime);
}

#[test]
fn each_artifact_class_becomes_eligible_after_its_own_age() {
    use ArtifactClass::*;
    let failed = |days| facts(RunState::Failed, days, None);
    assert_eq!(retained(&failed(3)), Retained::TooRecent);
    assert_eq!(prunable(&failed(7)), vec![Temporary]);
    assert_eq!(prunable(&failed(14)), vec![Temporary, Log]);
    assert_eq!(prunable(&failed(30)), vec![Temporary, Log, Checkpoint, Report]);
    assert_eq!(prunable(&failed(180)), vec![Temporary, Log, Checkpoint, Report, Metadata]);
}

#[test]
fn merged_and_closed_reports_wait_ninety_days() {
    use ArtifactClass::*;
    let merged = |days| facts(RunState::Completed, days, Some((ProviderState::Merged, 0, None)));
    assert_eq!(prunable(&merged(89)), vec![Temporary, Log, Checkpoint]);
    assert_eq!(prunable(&merged(90)), vec![Temporary, Log, Checkpoint, Report]);
    let closed = facts(RunState::Completed, 90, Some((ProviderState::Closed, 0, None)));
    assert_eq!(prunable(&closed), vec![Temporary, Log, Checkpoint, Report]);
}

#[test]
fn an_end_time_in_the_future_never_makes_anything_eligible() {
    let mut f = facts(RunState::Failed, 0, None);
    f.ended_at = Some(NOW + 400 * DAY);
    assert_eq!(retained(&f), Retained::TooRecent);
}
