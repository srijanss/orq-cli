use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use rusqlite::Connection;

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-ro-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

/// A database some other tool left at schema version 0.
fn unmigrated(roots: &StorageRoots) {
    let conn = Connection::open(roots.database()).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (x INTEGER);").unwrap();
}

fn version(roots: &StorageRoots) -> u32 {
    Connection::open(roots.database()).unwrap().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap()
}

#[test]
fn listing_reviews_never_migrates_the_database() {
    let roots = roots("list");
    unmigrated(&roots);
    assert!(inbox(&roots, "p-00000000", "p").is_err());
    assert_eq!(version(&roots), 0);
}

#[test]
fn opening_a_review_never_migrates_the_database() {
    let roots = roots("detail");
    unmigrated(&roots);
    assert!(detail(&roots, "run-1").is_err());
    assert_eq!(version(&roots), 0);
}
