//! Each feature's kind, standard or frontend, kept in orq.db by project and feature name.

use std::path::{Path, PathBuf};

use orq_core::store::{FeatureKind, Store};

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-feature-kind-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_kind_set_reads_back_per_project_and_survives_a_restart_and_standard_is_the_default() {
    let db = scratch("set").join("orq.db");
    let (alpha, beta) = (Path::new("/code/alpha"), Path::new("/code/beta"));
    {
        let store = Store::open(&db).unwrap();
        assert_eq!(store.feature_kind(alpha, "page").unwrap(), FeatureKind::Standard);
        store.set_feature_kind(alpha, "page", FeatureKind::Frontend).unwrap();
        store.set_feature_kind(alpha, "shop", FeatureKind::Frontend).unwrap();
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(store.feature_kind(alpha, "page").unwrap(), FeatureKind::Frontend);
    assert_eq!(store.feature_kind(beta, "page").unwrap(), FeatureKind::Standard, "another project's feature of the same name is its own");
    assert_eq!(store.frontend_features(alpha).unwrap(), ["page".to_string(), "shop".to_string()]);
    assert!(store.frontend_features(beta).unwrap().is_empty());

    store.set_feature_kind(alpha, "page", FeatureKind::Standard).unwrap();
    assert_eq!(store.feature_kind(alpha, "page").unwrap(), FeatureKind::Standard);
    assert_eq!(store.frontend_features(alpha).unwrap(), ["shop".to_string()]);
}

#[test]
fn only_standard_and_frontend_are_kinds() {
    assert_eq!(FeatureKind::parse("frontend"), Some(FeatureKind::Frontend));
    assert_eq!(FeatureKind::parse("standard"), Some(FeatureKind::Standard));
    for unknown in ["", "Frontend", "backend", "frontend "] {
        assert_eq!(FeatureKind::parse(unknown), None, "{unknown:?}");
    }
    assert_eq!(FeatureKind::Frontend.as_str(), "frontend");
    assert_eq!(FeatureKind::Standard.as_str(), "standard");
    assert_eq!(FeatureKind::default(), FeatureKind::Standard);
}

#[test]
fn the_read_only_lookup_writes_nothing_and_reads_an_older_database_as_having_no_frontend_features() {
    let db = scratch("read-only").join("orq.db");
    let alpha = Path::new("/code/alpha");
    Store::open(&db).unwrap().set_feature_kind(alpha, "page", FeatureKind::Frontend).unwrap();
    assert_eq!(Store::read_frontend_features(&db, alpha).unwrap(), ["page".to_string()]);

    let version = |db: &Path| -> u32 { rusqlite::Connection::open(db).unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap() };
    let before_kinds = Store::LATEST_VERSION - 1;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE feature_kinds RENAME TO feature_kinds_old;").unwrap();
    conn.pragma_update(None, "user_version", before_kinds).unwrap();
    drop(conn);
    assert!(Store::read_frontend_features(&db, alpha).unwrap().is_empty());
    assert_eq!(version(&db), before_kinds, "left as it was");
    assert!(Store::read_frontend_features(&db, Path::new("code/alpha")).is_err(), "a relative root is still refused");

    rusqlite::Connection::open(&db).unwrap().pragma_update(None, "user_version", Store::LATEST_VERSION + 1).unwrap();
    assert!(Store::read_frontend_features(&db, alpha).is_err(), "a newer schema is not guessed at");
}

#[test]
fn a_relative_project_root_is_refused() {
    let store = Store::open(&scratch("relative").join("orq.db")).unwrap();
    assert!(store.set_feature_kind(Path::new("code/alpha"), "page", FeatureKind::Frontend).is_err());
    assert!(store.feature_kind(Path::new("code/alpha"), "page").is_err());
}
