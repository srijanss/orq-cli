use std::path::PathBuf;

use orq_core::store::Store;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orq-publish-rollback-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn reject_inserts(db: &std::path::Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TRIGGER no_inserts BEFORE INSERT ON reviews BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;").unwrap();
}

#[test]
fn a_failed_indexing_restores_the_previous_report_and_index() {
    let root = dir("rebuild");
    let db = root.join("orq.db");
    let store = Store::open(&db).unwrap();
    let first = store.save_review(&root, "run-1", "proj", "feat", HEAD, br#"{"v":1}"#).unwrap();
    let path = root.join(&first.report_path);

    reject_inserts(&db);
    let failed = store.save_review(&root, "run-1", "proj", "feat", HEAD, br#"{"v":2}"#);
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), br#"{"v":1}"#);
    assert_eq!(store.reviews_for_run("run-1").unwrap(), vec![first]);
}

#[test]
fn a_failed_first_indexing_leaves_no_report_file() {
    let root = dir("first");
    let db = root.join("orq.db");
    let store = Store::open(&db).unwrap();
    reject_inserts(&db);

    assert!(store.save_review(&root, "run-2", "proj", "feat", HEAD, br#"{"v":1}"#).is_err());
    assert!(!root.join(format!("runs/run-2/review/{HEAD}/review.json")).exists());
    assert!(store.reviews_for_run("run-2").unwrap().is_empty());
}
