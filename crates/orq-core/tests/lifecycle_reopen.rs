use orq_core::review_lifecycle::{Gate, Observation, ProviderState, ReviewLifecycle};

const HEAD: &str = "1111111111111111111111111111111111111111";

fn seen(state: ProviderState) -> Result<Observation, orq_core::review_lifecycle::ObservationError> {
    Ok(Observation { state, head_sha: HEAD.to_string() })
}

#[test]
fn a_closed_review_that_is_reopened_waits_for_a_person_again() {
    let open = ReviewLifecycle::associate("github", "7", "https://h/o/r/pull/7", HEAD);
    let closed = open.apply(seen(ProviderState::Closed));
    assert_eq!(closed.gate, Gate::Resolved);
    let reopened = closed.apply(seen(ProviderState::Open));
    assert_eq!(reopened.state, ProviderState::Open);
    assert_eq!(reopened.gate, Gate::AwaitingHumanReview);
}

#[test]
fn a_merge_stays_final_even_if_the_provider_later_says_open() {
    let merged = ReviewLifecycle::associate("github", "7", "https://h/o/r/pull/7", HEAD).apply(seen(ProviderState::Merged));
    let again = merged.apply(seen(ProviderState::Open));
    assert_eq!(again.state, ProviderState::Merged);
    assert_eq!(again.gate, Gate::Resolved);
}
