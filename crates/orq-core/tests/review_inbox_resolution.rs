//! A review ends without a PR/MR: a local merge resolves it, old merged runs are backfilled once, and a
//! person can mark one reviewed, dismiss it or reopen it. The inbox status and the waiting count follow.

use orq_core::review_inbox::{self, waiting_count};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const OLD: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OURS: &str = "p-0a1b2c3d";

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-resolution-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

/// `run-1` with a report for an older head and then one for `HEAD`.
fn reported(tag: &str) -> (StorageRoots, Store) {
    let roots = roots(tag);
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", OURS, "COM-01", OLD, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_review(&roots.data, "run-1", OURS, "COM-01", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
    (roots, store)
}

fn status(roots: &StorageRoots) -> String {
    review_inbox::inbox(roots, OURS, "p").unwrap().remove(0).status
}

#[test]
fn a_local_merge_resolves_the_review_and_every_older_report_of_the_run() {
    let (roots, store) = reported("merged");
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("ready", 1));

    review_inbox::record_local_merge(&store, "run-1", HEAD).unwrap();

    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("merged", 0));
    let all = store.lifecycles_for_run("run-1").unwrap();
    let old = all.iter().find(|l| l.reported_head_sha == OLD).expect("the superseded report is resolved too");
    assert_eq!((old.provider.as_str(), old.state.as_str(), old.gate.as_str(), old.url.as_str(), old.observed), ("local", "merged", "resolved", "", true));
    assert_eq!(old.review_id, HEAD, "the lifecycle keeps the merged commit");
}

#[test]
fn a_repeated_local_merge_saves_nothing_and_a_provider_review_is_never_replaced() {
    use orq_core::review_lifecycle::ReviewLifecycle;
    let (_roots, store) = reported("repeat");
    let pr = ReviewLifecycle::associate("github", "7", "https://github.com/o/r/pull/7", OLD);
    store.save_lifecycle("run-1", &pr).unwrap();

    assert_eq!(review_inbox::record_local_merge(&store, "run-1", HEAD).unwrap(), 1, "only the head without a provider review");
    assert_eq!(review_inbox::record_local_merge(&store, "run-1", HEAD).unwrap(), 0, "a second pass saves nothing");

    let all = store.lifecycles_for_run("run-1").unwrap();
    assert_eq!(all.iter().find(|l| l.reported_head_sha == OLD), Some(&pr), "the PR's lifecycle is untouched");
    assert_eq!(all.len(), 2, "{all:?}");
}
#[test]
fn a_person_marks_a_review_reviewed_dismisses_it_and_reopens_it() {
    use review_inbox::{act_on_review, ActionOutcome, ReviewAction};
    let (roots, store) = reported("manual");
    let done = |action, head: Option<&str>| match act_on_review(&store, "run-1", action, head).unwrap() {
        ActionOutcome::Done(lifecycle) => lifecycle,
        other => panic!("{action:?} was not done: {other:?}"),
    };

    let resolved = done(ReviewAction::Resolve, Some(HEAD));
    assert_eq!((resolved.provider.as_str(), resolved.reported_head_sha.as_str()), ("local", HEAD));
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("resolved", 0));

    done(ReviewAction::Reopen, None);
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("ready", 1), "a reopened review waits again with its report's status");

    done(ReviewAction::Dismiss, Some(HEAD));
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("closed", 0));

    done(ReviewAction::Reopen, Some(HEAD));
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("ready", 1));
    assert_eq!(ReviewAction::Dismiss.event_kind(), "review.dismissed");
}

#[test]
fn an_action_on_an_older_head_an_unknown_run_or_a_merged_review_changes_nothing() {
    use review_inbox::{act_on_review, ActionOutcome, ReviewAction};
    let (roots, store) = reported("refused");

    let stale = act_on_review(&store, "run-1", ReviewAction::Resolve, Some(OLD)).unwrap();
    assert_eq!(stale, ActionOutcome::StaleHead { latest: HEAD.into() }, "the person saw an older report");
    assert_eq!(act_on_review(&store, "run-9", ReviewAction::Resolve, None).unwrap(), ActionOutcome::NoReview);
    assert_eq!(status(&roots), "ready");

    act_on_review(&store, "run-1", ReviewAction::Resolve, None).unwrap();
    review_inbox::record_local_merge(&store, "run-1", HEAD).unwrap();
    assert_eq!(status(&roots), "merged", "a merge after a manual resolve still records the merge");
    for action in [ReviewAction::Dismiss, ReviewAction::Reopen] {
        assert!(matches!(act_on_review(&store, "run-1", action, None).unwrap(), ActionOutcome::Refused(_)), "{action:?}");
    }
    assert_eq!(status(&roots), "merged");
    assert!(matches!(act_on_review(&store, "run-1", ReviewAction::Resolve, None).unwrap(), ActionOutcome::Done(_)), "resolving a merged review is a no-op, not an error");
}

#[test]
fn old_local_merges_are_backfilled_once_and_a_run_without_a_report_is_left_alone() {
    let (roots, store) = reported("backfill");
    let merges = vec![("run-1".to_string(), HEAD.to_string()), ("run-7".to_string(), OLD.to_string())];

    assert_eq!(review_inbox::backfill_local_merges(&store, &merges).unwrap(), 1, "only run-1 has a review");
    assert_eq!(review_inbox::backfill_local_merges(&store, &merges).unwrap(), 0, "a second backfill changes nothing");
    assert_eq!((status(&roots).as_str(), waiting_count(&store, OURS).unwrap()), ("merged", 0));
    assert!(store.lifecycles_for_run("run-7").unwrap().is_empty());
}

#[test]
fn one_run_s_status_is_the_one_its_inbox_row_shows() {
    use orq_core::review_inbox::{ReviewAction, act_on_review};
    let (roots, store) = reported("one");
    store.save_review(&roots.data, "run-2", "p-other", "COM-02", HEAD, br#"{"status":"draft","risks":[]}"#).unwrap();
    assert_eq!(review_inbox::status_of_run(&roots, "run-1").unwrap().as_deref(), Some("ready"));
    assert_eq!(review_inbox::status_of_run(&roots, "run-2").unwrap().as_deref(), Some("draft"));

    act_on_review(&store, "run-1", ReviewAction::Resolve, Some(HEAD)).unwrap();
    assert_eq!(review_inbox::status_of_run(&roots, "run-1").unwrap(), Some(status(&roots)));
    assert_eq!(status(&roots), "resolved");
    assert_eq!(review_inbox::status_of_run(&roots, "run-9").unwrap(), None, "a run without a report has no status");
}
