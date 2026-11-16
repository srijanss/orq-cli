//! The PR/MR a run created or adopted is kept as the run's provider review: when it is created (seen
//! open at the pushed head) and, for runs from before that, by a backfill that records it unverified.

use orq_core::review_inbox::{self, CreatedReview};
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const OLD: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const URL: &str = "https://github.com/o/r/pull/7";

fn store(tag: &str) -> Store {
    let base = orq_test_support::process_temp_dir().join(format!("orq-provider-record-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-1", "COM-01", OLD, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_review(&roots.data, "run-1", "p-1", "COM-01", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
    store
}

fn created(run_id: &str) -> CreatedReview {
    CreatedReview { run_id: run_id.into(), provider: "github".into(), review_id: "7".into(), url: URL.into() }
}

#[test]
fn a_backfilled_review_is_recorded_unverified_for_the_latest_report_once() {
    let store = store("backfill");
    let reviews = [created("run-1"), created("run-without-report")];

    assert_eq!(review_inbox::backfill_created_reviews(&store, &reviews).unwrap(), 1, "only the run with a report");
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![ReviewLifecycle::associate("github", "7", URL, HEAD)]);
    assert!(store.lifecycles_for_run("run-without-report").unwrap().is_empty());

    assert_eq!(review_inbox::backfill_created_reviews(&store, &reviews).unwrap(), 0, "a second backfill changes nothing");
    assert_eq!(store.lifecycles_for_run("run-1").unwrap().len(), 1);
}

#[test]
fn a_run_that_already_has_a_provider_review_is_left_alone_but_a_local_one_is_not_enough() {
    let store = store("existing");
    let seen = ReviewLifecycle::associate("github", "9", "https://github.com/o/r/pull/9", OLD);
    store.save_lifecycle("run-1", &seen).unwrap();
    assert_eq!(review_inbox::backfill_created_reviews(&store, &[created("run-1")]).unwrap(), 0);
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![seen]);

    let other = self::store("local-only");
    other.save_lifecycle("run-1", &ReviewLifecycle::local(OLD)).unwrap();
    assert_eq!(review_inbox::backfill_created_reviews(&other, &[created("run-1")]).unwrap(), 1);
    assert!(other.lifecycles_for_run("run-1").unwrap().iter().any(|l| l.provider == "github" && l.reported_head_sha == HEAD && l.state == ProviderState::Open));
}

#[test]
fn a_report_a_person_already_acted_on_here_is_not_replaced() {
    let store = store("acted");
    let resolved = ReviewLifecycle::local(HEAD).resolved();
    store.save_lifecycle("run-1", &resolved).unwrap();
    assert_eq!(review_inbox::backfill_created_reviews(&store, &[created("run-1")]).unwrap(), 0);
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![resolved]);
}

#[test]
fn a_created_review_is_recorded_as_seen_open_at_the_pushed_head() {
    let store = store("created");
    review_inbox::record_created_review(&store, &created("run-1"), HEAD).unwrap();
    let all = store.lifecycles_for_run("run-1").unwrap();
    let mut expected = ReviewLifecycle::associate("github", "7", URL, HEAD);
    expected.observed = true;
    assert_eq!(all, vec![expected.clone()]);
    assert!(store.lifecycle_checked_at("run-1", HEAD).unwrap().is_some(), "it is checked now, so it reads as current");

    // Adopted again on a resumed run: a person's "reviewed" on that head is kept.
    store.save_lifecycle("run-1", &expected.resolved()).unwrap();
    review_inbox::record_created_review(&store, &created("run-1"), HEAD).unwrap();
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![expected.resolved()]);
}

#[test]
fn an_adopted_review_is_recorded_unverified_since_it_may_already_be_merged() {
    let store = store("adopted");
    review_inbox::record_adopted_review(&store, &created("run-1"), HEAD).unwrap();
    let expected = ReviewLifecycle::associate("github", "7", URL, HEAD);
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![expected.clone()], "never looked at, so unverified until it is");

    // Adopted again on a resumed run: what is already known about it on that head is kept.
    store.save_lifecycle("run-1", &expected.resolved()).unwrap();
    review_inbox::record_adopted_review(&store, &created("run-1"), HEAD).unwrap();
    assert_eq!(store.lifecycles_for_run("run-1").unwrap(), vec![expected.resolved()]);
}
