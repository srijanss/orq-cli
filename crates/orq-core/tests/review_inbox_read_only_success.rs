use std::os::unix::fs::PermissionsExt;

use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn seeded(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-rosucc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"current","risks":[{"severity":"high","file":"a.rs","line":1,"claim":"x"}]}"#).unwrap();
    drop(store);
    roots
}

fn snapshot(roots: &StorageRoots) -> (u64, std::time::SystemTime, bool, String) {
    let meta = std::fs::metadata(roots.database()).unwrap();
    let mode = rusqlite::Connection::open(roots.database()).unwrap().pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0)).unwrap();
    let wal = roots.database().with_extension("db-wal").exists();
    (meta.len(), meta.modified().unwrap(), wal, mode)
}

#[test]
fn a_current_schema_database_without_write_permission_is_read_and_left_untouched() {
    let roots = seeded("ro");
    let db = roots.database();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = snapshot(&roots);

    let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!((rows[0].status.as_str(), rows[0].risk.as_str(), rows[0].current), ("ready", "high", true), "{rows:?}");
    let d = detail(&roots, "run-1").unwrap();
    assert!(d.risks.join("\n").contains("high"), "{:?}", d.risks);

    assert_eq!(snapshot(&roots), before);
    assert_eq!(std::fs::metadata(&db).unwrap().permissions().mode() & 0o777, 0o444);
}
