use std::os::unix::fs::PermissionsExt;

use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn seeded(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-inbox-rosucc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"current","risks":[{"severity":"high","file":"a.rs","line":1,"claim":"x"}]}"#).unwrap();
    drop(store);
    roots
}

/// Size and mtime of the database and its sidecars, read from the filesystem
/// only, so taking the snapshot cannot itself touch the storage.
fn snapshot(roots: &StorageRoots) -> Vec<Option<(u64, std::time::SystemTime)>> {
    let db = roots.database();
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut name = db.clone().into_os_string();
            name.push(suffix);
            std::fs::metadata(name).ok().map(|m| (m.len(), m.modified().unwrap()))
        })
        .collect()
}

fn journal_mode(roots: &StorageRoots) -> String {
    let conn = rusqlite::Connection::open_with_flags(roots.database(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0)).unwrap()
}

fn read_everything(roots: &StorageRoots) {
    let rows = inbox(roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!((rows[0].status.as_str(), rows[0].risk.as_str(), rows[0].current), ("ready", "high", true), "{rows:?}");
    let d = detail(roots, "run-1").unwrap();
    assert!(d.risks.join("\n").contains("high"), "{:?}", d.risks);
}

#[test]
fn a_current_schema_database_without_write_permission_is_read_and_left_untouched() {
    let roots = seeded("ro");
    let db = roots.database();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = snapshot(&roots);
    read_everything(&roots);
    let after = snapshot(&roots);
    // The database file itself never changes. SQLite may leave empty -wal/-shm
    // sidecars behind from a reader, but the log must hold no writes.
    assert_eq!(after[0], before[0]);
    assert!(after[1].map_or(true, |(len, _)| len == 0), "{after:?}");
    assert_eq!(std::fs::metadata(&db).unwrap().permissions().mode() & 0o777, 0o444);
    assert_eq!(journal_mode(&roots), "wal");
}

#[test]
fn a_database_in_a_directory_nobody_can_write_to_is_still_read() {
    let roots = seeded("rodir");
    let db = roots.database();
    let dir = db.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let before = snapshot(&roots);
    read_everything(&roots);
    let after = snapshot(&roots);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(after, before);
}
