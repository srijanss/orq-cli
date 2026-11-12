use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, std::path::PathBuf, StorageRoots) {
    let base = std::env::temp_dir().join(format!("orq-pins-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let db = roots.database();
    (Store::open(&db).unwrap(), db, roots)
}

#[test]
fn a_run_is_pinned_until_it_is_unpinned() {
    let (store, _, _) = open("toggle");
    assert!(!store.is_pinned("run-1").unwrap());
    store.pin("run-1").unwrap();
    assert!(store.is_pinned("run-1").unwrap());
    assert!(!store.is_pinned("run-2").unwrap());
    store.unpin("run-1").unwrap();
    assert!(!store.is_pinned("run-1").unwrap());
}

#[test]
fn pinning_and_unpinning_twice_is_harmless() {
    let (store, _, _) = open("idem");
    store.pin("run-1").unwrap();
    store.pin("run-1").unwrap();
    assert_eq!(store.pinned().unwrap(), vec!["run-1".to_string()]);
    store.unpin("run-1").unwrap();
    store.unpin("run-1").unwrap();
    assert!(store.pinned().unwrap().is_empty());
}

#[test]
fn pins_survive_reopening_and_list_sorted() {
    let (store, db, _) = open("reopen");
    store.pin("run-b").unwrap();
    store.pin("run-a").unwrap();
    drop(store);
    let again = Store::open(&db).unwrap();
    assert_eq!(again.pinned().unwrap(), vec!["run-a".to_string(), "run-b".to_string()]);
}

#[test]
fn an_unsafe_run_id_cannot_be_pinned() {
    let (store, _, _) = open("unsafe");
    assert!(store.pin("../x").is_err());
    assert!(store.pin("").is_err());
    assert!(store.pinned().unwrap().is_empty());
}

#[test]
fn a_version_two_database_is_upgraded_keeping_its_reviews() {
    let (store, db, roots) = open("upgrade");
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings RENAME TO project_settings_old; ALTER TABLE run_pins RENAME TO run_pins_old; ALTER TABLE prune_journal RENAME TO prune_journal_old; ALTER TABLE review_lifecycles DROP COLUMN observed;").unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    // Version 2 has no pin table; opening must create it without touching reviews.
    let store = Store::open(&db).unwrap();
    store.pin("run-1").unwrap();
    assert_eq!(store.all_reviews().unwrap().len(), 1);
    assert!(store.is_pinned("run-1").unwrap());
}
