use orq_core::review_lifecycle::{Gate, Observation, ObservationError, ProviderState, ReviewLifecycle};

const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

fn associated() -> ReviewLifecycle {
    ReviewLifecycle::associate("github", "7", "https://example.test/o/r/pull/7", HEAD)
}

fn seen(state: ProviderState, head: &str) -> Result<Observation, ObservationError> {
    Ok(Observation { state, head_sha: head.to_string() })
}

#[test]
fn an_associated_review_awaits_a_human_on_the_reported_commit() {
    let review = associated();
    assert_eq!(review.reported_head_sha, HEAD);
    assert_eq!(review.state, ProviderState::Open);
    assert_eq!(review.gate, Gate::AwaitingHumanReview);
    assert!(!review.stale);
    assert!(review.last_error.is_none());
}

#[test]
fn an_open_review_on_the_same_commit_stays_current() {
    let review = associated().apply(seen(ProviderState::Open, HEAD));
    assert!(!review.stale);
    assert_eq!(review.gate, Gate::AwaitingHumanReview);
}

#[test]
fn a_changed_head_marks_the_report_stale_but_keeps_the_original_commit() {
    let review = associated().apply(seen(ProviderState::Open, OTHER));
    assert!(review.stale);
    assert_eq!(review.reported_head_sha, HEAD);
    assert_eq!(review.observed_head_sha.as_deref(), Some(OTHER));
    assert_eq!(review.gate, Gate::AwaitingHumanReview);
}

#[test]
fn a_manual_merge_is_observed_and_ends_the_gate() {
    let review = associated().apply(seen(ProviderState::Merged, HEAD));
    assert_eq!(review.state, ProviderState::Merged);
    assert_eq!(review.gate, Gate::Resolved);
}

#[test]
fn a_closed_unmerged_review_ends_the_gate_without_being_merged() {
    let review = associated().apply(seen(ProviderState::Closed, HEAD));
    assert_eq!(review.state, ProviderState::Closed);
    assert_eq!(review.gate, Gate::Resolved);
}

#[test]
fn a_provider_error_keeps_the_last_known_state_and_the_gate() {
    let review = associated().apply(Err(ObservationError::Unavailable("timeout".into())));
    assert_eq!(review.state, ProviderState::Open);
    assert_eq!(review.gate, Gate::AwaitingHumanReview);
    assert!(review.last_error.as_deref().unwrap().contains("timeout"));
}

#[test]
fn a_permission_error_is_told_apart_from_an_outage() {
    let review = associated().apply(Err(ObservationError::PermissionDenied("403".into())));
    let message = review.last_error.unwrap();
    assert!(message.contains("permission"), "{message}");
}

#[test]
fn a_later_good_observation_clears_the_error() {
    let review = associated().apply(Err(ObservationError::Unavailable("down".into()))).apply(seen(ProviderState::Open, HEAD));
    assert!(review.last_error.is_none());
}

#[test]
fn a_merge_is_final_even_if_the_provider_later_reports_open() {
    let review = associated().apply(seen(ProviderState::Merged, HEAD)).apply(seen(ProviderState::Open, OTHER));
    assert_eq!(review.state, ProviderState::Merged);
    assert_eq!(review.gate, Gate::Resolved);
}
