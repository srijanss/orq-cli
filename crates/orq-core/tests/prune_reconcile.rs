use orq_core::prune::{reconcile, Reconciled};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, StorageRoots) {
    let base = std::env::temp_dir().join(format!("orq-reconcile-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    (Store::open(&roots.database()).unwrap(), roots)
}

#[test]
fn a_removal_interrupted_after_the_file_went_is_finished() {
    let (store, roots) = open("gone");
    let record = store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    store.begin_removal("run-1", &record.report_path).unwrap();
    std::fs::remove_file(roots.data.join(&record.report_path)).unwrap();
    let done = reconcile(&roots.data, &store).unwrap();
    assert_eq!(done, Reconciled { completed: vec![record.report_path.clone()], abandoned: vec![] });
    assert!(store.pending_removals().unwrap().is_empty());
    assert!(store.reviews_for_run("run-1").unwrap().is_empty(), "the index no longer points at a missing report");
}

#[test]
fn a_removal_interrupted_before_the_file_went_leaves_file_and_index_alone() {
    let (store, roots) = open("present");
    let record = store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    store.begin_removal("run-1", &record.report_path).unwrap();
    let done = reconcile(&roots.data, &store).unwrap();
    assert_eq!(done.abandoned, [record.report_path.clone()]);
    assert!(roots.data.join(&record.report_path).exists());
    assert_eq!(store.reviews_for_run("run-1").unwrap().len(), 1);
    assert!(store.pending_removals().unwrap().is_empty());
}

#[test]
fn reconciling_with_nothing_pending_does_nothing_and_twice_is_harmless() {
    let (store, roots) = open("empty");
    assert_eq!(reconcile(&roots.data, &store).unwrap(), Reconciled::default());
    store.begin_removal("run-1", "runs/run-1/events.jsonl").unwrap();
    reconcile(&roots.data, &store).unwrap();
    assert_eq!(reconcile(&roots.data, &store).unwrap(), Reconciled::default());
}

#[test]
fn a_journal_entry_that_escapes_the_data_root_is_dropped_without_touching_anything() {
    let (store, roots) = open("escape");
    let outside = roots.data.parent().unwrap().join("secret.txt");
    std::fs::write(&outside, "keep").unwrap();
    store.begin_removal("run-1", "runs/run-1/../../../secret.txt").unwrap();
    let done = reconcile(&roots.data, &store).unwrap();
    assert!(outside.exists());
    assert_eq!(done.abandoned.len(), 1);
    assert!(store.pending_removals().unwrap().is_empty());
}
