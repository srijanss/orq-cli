//! An upgrade that only adds to orq.db leaves older Orq binaries using it; one that changes what they read refuses them.

use std::path::PathBuf;

use orq_core::store::{ReviewRecord, Store};

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-schema-compat-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("orq.db")
}

fn record(run: &str) -> ReviewRecord {
    let sha = "49485ef0000000000000000000000000000000aa";
    ReviewRecord {
        run_id: run.into(),
        project: "alpha".into(),
        feature: "login".into(),
        head_sha: sha.into(),
        report_path: format!("runs/{run}/review/{sha}/review.json"),
    }
}

/// What a newer Orq leaves after its next migration: a new table, the version, and the marker it sets.
fn upgrade_as_newer_orq(db: &std::path::Path, breaking: bool) -> u32 {
    let newer = Store::LATEST_VERSION + 1;
    let conn = rusqlite::Connection::open(db).unwrap();
    let compat: u32 = conn.query_row("SELECT compat_from FROM schema_compat", [], |row| row.get(0)).unwrap();
    conn.execute_batch("CREATE TABLE added_later (x INTEGER)").unwrap();
    conn.execute("UPDATE schema_compat SET version = ?1, compat_from = ?2", [newer, if breaking { newer } else { compat }]).unwrap();
    conn.pragma_update(None, "user_version", newer).unwrap();
    newer
}

#[test]
fn version_10_is_additive_so_a_schema_9_orq_keeps_recording_reviews_in_a_schema_10_database() {
    let db = scratch("v10");
    drop(Store::open(&db).unwrap());
    assert_eq!(Store::LATEST_VERSION, 10);
    assert!(!orq_core::store::is_breaking(9));
    assert!(!orq_core::store::is_breaking(10));

    let older = Store::open_understanding(&db, 9).unwrap();
    older.record_review(&record("r-older")).unwrap();
    assert_eq!(older.schema_version().unwrap(), 10, "an older Orq never downgrades the database");
    drop(older);
    assert_eq!(Store::open(&db).unwrap().reviews_for_run("r-older").unwrap().len(), 1);
}

#[test]
fn a_database_upgraded_by_an_additive_migration_opens_and_records_reviews() {
    let db = scratch("additive");
    drop(Store::open(&db).unwrap());
    let newer = upgrade_as_newer_orq(&db, false);

    let store = Store::open(&db).unwrap();
    store.record_review(&record("r1")).unwrap();
    assert_eq!(store.schema_version().unwrap(), newer);
    drop(store);
    assert!(Store::open_read_only(&db).is_ok(), "reading needs no newer Orq either");
}

#[test]
fn a_database_upgraded_by_a_breaking_migration_is_refused_naming_both_versions() {
    let db = scratch("breaking");
    drop(Store::open(&db).unwrap());
    let newer = upgrade_as_newer_orq(&db, true);

    for err in [Store::open(&db).err(), Store::open_read_only(&db).err()] {
        let message = err.expect("a breaking newer schema is refused").to_string();
        assert!(message.contains(&newer.to_string()), "{message}");
        assert!(message.contains(&Store::LATEST_VERSION.to_string()), "{message}");
        assert!(message.contains("restart runs started before the upgrade"), "{message}");
    }
    let settings = Store::read_project_settings(&db, std::path::Path::new("/repo"));
    assert!(settings.is_err(), "settings are not read from a schema this Orq cannot use");
}

#[test]
fn compat_stays_where_the_last_breaking_migration_left_it_across_additive_ones() {
    let db = scratch("compat");
    let store = Store::open(&db).unwrap();
    let last_breaking = (1..=Store::LATEST_VERSION).filter(|v| orq_core::store::is_breaking(*v)).max().unwrap();
    assert!(last_breaking < Store::LATEST_VERSION, "the newest migrations only add");
    assert_eq!(store.compat_from().unwrap(), Some(last_breaking));
    drop(store);

    let newer = upgrade_as_newer_orq(&db, false);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.compat_from().unwrap(), Some(last_breaking));
    assert_eq!(store.schema_version().unwrap(), newer);
}

#[test]
fn a_newer_database_without_a_marker_for_its_version_is_still_refused() {
    let db = scratch("unmarked");
    drop(Store::open(&db).unwrap());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", Store::LATEST_VERSION + 1).unwrap();
    drop(conn);
    assert!(Store::open(&db).is_err());
    assert!(Store::open_read_only(&db).is_err());
}
