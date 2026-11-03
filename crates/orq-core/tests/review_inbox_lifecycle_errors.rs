use orq_core::review_inbox::inbox;
use orq_core::review_lifecycle::ReviewLifecycle;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-life-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn seeded(tag: &str) -> StorageRoots {
    let roots = roots(tag);
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_lifecycle("run-1", &ReviewLifecycle::associate("github", "7", "https://example.test/7", HEAD)).unwrap();
    roots
}

fn corrupt_lifecycle(roots: &StorageRoots) {
    let conn = rusqlite::Connection::open(roots.database()).unwrap();
    conn.execute("UPDATE review_lifecycles SET state = 'bogus'", []).unwrap();
}

#[test]
fn a_lifecycle_that_cannot_be_read_is_unknown_and_never_shown_as_fresh() {
    let roots = seeded("corrupt");
    corrupt_lifecycle(&roots);
    let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "unknown");
    assert!(rows[0].stale, "unknown provider state must not look fresh");
}

#[test]
fn an_unreadable_report_stays_unreadable_when_a_lifecycle_exists() {
    let roots = seeded("unreadable");
    std::fs::write(roots.data.join(format!("runs/run-1/review/{HEAD}/review.json")), b"{ nope").unwrap();
    let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!(rows[0].status, "unreadable");
}
