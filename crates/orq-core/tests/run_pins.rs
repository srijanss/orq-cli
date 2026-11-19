//! Run pins were removed. Upgrading drops the `run_pins` table when it is empty; one with pins in
//! it is kept until `take_removed_pins` has counted them for the one-time note, then dropped.

use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, std::path::PathBuf, StorageRoots) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-pins-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let db = roots.database();
    (Store::open(&db).unwrap(), db, roots)
}

fn has_pins_table(db: &std::path::Path) -> bool {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_pins')", [], |row| row.get(0)).unwrap()
}

/// Puts the database back at version 6, as an Orq from before pins were removed left it (without
/// the run limit columns version 8 added, the run queue version 9 added or the review wait version 10 added).
fn back_to_version_six(db: &std::path::Path, pins: &[&str]) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("CREATE TABLE run_pins (run_id TEXT NOT NULL PRIMARY KEY, pinned_at INTEGER NOT NULL); \
         ALTER TABLE project_settings DROP COLUMN max_active_runs; ALTER TABLE project_settings DROP COLUMN min_free_disk_gb; \
         ALTER TABLE project_settings DROP COLUMN queue_hours; ALTER TABLE project_settings DROP COLUMN queue_stop_on_failure; ALTER TABLE project_settings DROP COLUMN queue_wait_for_review; \
         ALTER TABLE project_settings DROP COLUMN pull_target_before_runs; \
         ALTER TABLE run_queue RENAME TO run_queue_old; ALTER TABLE run_queue_paused RENAME TO run_queue_paused_old; \
         ALTER TABLE todos RENAME TO todos_old; ALTER TABLE queue_wishes RENAME TO queue_wishes_old; \
         PRAGMA user_version = 6;").unwrap();
    for pin in pins {
        conn.execute("INSERT INTO run_pins (run_id, pinned_at) VALUES (?1, 1)", [pin]).unwrap();
    }
}

#[test]
fn a_new_database_has_no_pins_table_and_nothing_to_tell() {
    let (store, db, _) = open("fresh");
    assert!(!has_pins_table(&db));
    assert_eq!(store.take_removed_pins().unwrap(), 0);
}

#[test]
fn an_empty_pins_table_is_dropped_on_upgrade() {
    let (store, db, _) = open("empty");
    drop(store);
    back_to_version_six(&db, &[]);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), Store::LATEST_VERSION);
    assert!(!has_pins_table(&db));
    assert_eq!(store.take_removed_pins().unwrap(), 0);
}

#[test]
fn a_pins_table_with_pins_is_kept_on_upgrade_until_they_are_counted_once() {
    let (store, db, _) = open("kept");
    drop(store);
    back_to_version_six(&db, &["run-a", "run-b"]);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), Store::LATEST_VERSION);
    assert!(has_pins_table(&db), "kept for the note");
    assert_eq!(store.take_removed_pins().unwrap(), 2);
    assert!(!has_pins_table(&db));
    assert_eq!(store.take_removed_pins().unwrap(), 0, "counted once");
}

#[test]
fn pins_another_process_is_already_taking_are_not_counted_again() {
    let (store, db, _) = open("race");
    drop(store);
    back_to_version_six(&db, &["run-a", "run-b"]);
    let store = Store::open(&db).unwrap();
    // Another `storage status` has counted the pins and dropped the table, and not yet committed.
    let other = rusqlite::Connection::open(&db).unwrap();
    other.execute_batch("BEGIN IMMEDIATE; DROP TABLE run_pins;").unwrap();
    let committer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        other.execute_batch("COMMIT").unwrap();
    });
    let taken = store.take_removed_pins();
    committer.join().unwrap();
    assert_eq!(taken.unwrap(), 0, "the other process already told about these pins");
    assert!(!has_pins_table(&db));
}

#[test]
fn a_version_two_database_is_upgraded_keeping_its_reviews_and_no_pins_table() {
    let (store, db, roots) = open("upgrade");
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings RENAME TO project_settings_old; ALTER TABLE prune_journal RENAME TO prune_journal_old; ALTER TABLE review_lifecycles DROP COLUMN observed; ALTER TABLE run_queue RENAME TO run_queue_old; ALTER TABLE run_queue_paused RENAME TO run_queue_paused_old; ALTER TABLE todos RENAME TO todos_old; ALTER TABLE queue_wishes RENAME TO queue_wishes_old;").unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.all_reviews().unwrap().len(), 1);
    assert!(!has_pins_table(&db), "the table version 3 creates is empty, so it is dropped again");
}
