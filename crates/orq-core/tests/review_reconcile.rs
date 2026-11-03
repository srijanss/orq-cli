use orq_core::review_lifecycle::{reconcile, Candidate, ProviderState};

fn candidate(id: &str, state: ProviderState) -> Candidate {
    Candidate { review_id: id.into(), url: format!("https://example.test/pull/{id}"), state }
}

#[test]
fn no_candidates_means_nothing_to_track() {
    let outcome = reconcile(&[], None);
    assert!(outcome.primary.is_none());
    assert!(outcome.duplicates.is_empty());
}

#[test]
fn the_recorded_review_wins_while_it_is_open() {
    let found = [candidate("5", ProviderState::Open), candidate("9", ProviderState::Open)];
    let outcome = reconcile(&found, Some("5"));
    assert_eq!(outcome.primary.unwrap().review_id, "5");
    assert_eq!(outcome.duplicates.iter().map(|c| c.review_id.as_str()).collect::<Vec<_>>(), ["9"]);
}

#[test]
fn without_a_usable_recorded_review_the_newest_open_one_wins() {
    let found = [candidate("5", ProviderState::Open), candidate("12", ProviderState::Open), candidate("9", ProviderState::Open)];
    let outcome = reconcile(&found, None);
    assert_eq!(outcome.primary.unwrap().review_id, "12");
    assert_eq!(outcome.duplicates.len(), 2);
}

#[test]
fn ids_compare_as_numbers_not_text() {
    let found = [candidate("9", ProviderState::Open), candidate("10", ProviderState::Open)];
    assert_eq!(reconcile(&found, None).primary.unwrap().review_id, "10");
}

#[test]
fn an_open_review_beats_a_merged_one_and_a_merged_one_beats_a_closed_one() {
    let found = [candidate("3", ProviderState::Merged), candidate("2", ProviderState::Open), candidate("8", ProviderState::Closed)];
    assert_eq!(reconcile(&found, None).primary.unwrap().review_id, "2");
    let found = [candidate("3", ProviderState::Merged), candidate("8", ProviderState::Closed)];
    assert_eq!(reconcile(&found, None).primary.unwrap().review_id, "3");
}

#[test]
fn closed_reviews_alone_are_still_reported_as_closed_not_dropped() {
    let found = [candidate("4", ProviderState::Closed)];
    let outcome = reconcile(&found, None);
    assert_eq!(outcome.primary.unwrap().state, ProviderState::Closed);
}

#[test]
fn a_recorded_review_that_was_merged_still_wins_over_a_newer_open_one() {
    let found = [candidate("5", ProviderState::Merged), candidate("9", ProviderState::Open)];
    assert_eq!(reconcile(&found, Some("5")).primary.unwrap().review_id, "5");
}
