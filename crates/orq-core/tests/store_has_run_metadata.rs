use orq_core::review_lifecycle::ReviewLifecycle;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, StorageRoots) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-hasmeta-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    (Store::open(&roots.database()).unwrap(), roots)
}

#[test]
fn a_run_with_nothing_stored_has_no_metadata() {
    let (store, _) = open("none");
    assert!(!store.has_run_metadata("run-1").unwrap());
}

#[test]
fn a_review_row_alone_counts() {
    let (store, roots) = open("review");
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    assert!(store.has_run_metadata("run-1").unwrap());
    assert!(!store.has_run_metadata("run-2").unwrap());
}

#[test]
fn a_lifecycle_row_alone_counts_even_after_the_review_rows_are_gone() {
    let (store, _) = open("lifecycle");
    store.save_lifecycle("run-1", &ReviewLifecycle::associate("github", "7", "https://example.test/pr/7", &"a".repeat(40))).unwrap();
    assert!(store.reviews_for_run("run-1").unwrap().is_empty());
    assert!(store.has_run_metadata("run-1").unwrap());
    store.remove_run_metadata("run-1").unwrap();
    assert!(!store.has_run_metadata("run-1").unwrap());
}
