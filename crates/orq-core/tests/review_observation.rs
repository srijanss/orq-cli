use orq_core::review_lifecycle::{Gate, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> Store {
    let base = orq_test_support::process_temp_dir().join(format!("orq-obs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Store::open(&roots.database()).unwrap()
}

fn lifecycle(state: ProviderState, error: Option<&str>) -> ReviewLifecycle {
    ReviewLifecycle {
        provider: "github".into(),
        review_id: "7".into(),
        url: "https://example.test/pr/7".into(),
        reported_head_sha: "a".repeat(40),
        observed_head_sha: None,
        state,
        gate: Gate::AwaitingHumanReview,
        stale: false,
        last_error: error.map(String::from),
        observed: true,
    }
}

#[test]
fn a_run_with_no_observed_review_has_no_observation() {
    let store = open("none");
    assert!(store.review_observation("run-1").unwrap().is_none());
}

#[test]
fn the_observation_carries_state_time_and_error() {
    let store = open("one");
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    store.save_lifecycle("run-1", &lifecycle(ProviderState::Merged, Some("rate limited"))).unwrap();
    let seen = store.review_observation("run-1").unwrap().unwrap();
    assert_eq!(seen.state, ProviderState::Merged);
    assert!(seen.observed_at >= before && seen.observed_at <= before + 60, "{}", seen.observed_at);
    assert_eq!(seen.last_error.as_deref(), Some("rate limited"));
}

#[test]
fn an_unsafe_run_id_is_refused() {
    let store = open("unsafe");
    assert!(store.review_observation("../x").is_err());
}
