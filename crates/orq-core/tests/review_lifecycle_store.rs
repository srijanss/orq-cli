use std::path::PathBuf;

use orq_core::review_lifecycle::{Gate, Observation, ObservationError, ProviderState, ReviewLifecycle};
use orq_core::store::Store;

const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-lifecycle-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn associated() -> ReviewLifecycle {
    ReviewLifecycle::associate("gitlab", "7", "https://example.test/o/r/-/merge_requests/7", HEAD)
}

#[test]
fn a_lifecycle_survives_a_restart() {
    let db = scratch("restart").join("orq.db");
    let stale = associated().apply(Ok(Observation { state: ProviderState::Open, head_sha: OTHER.into() }));
    Store::open(&db).unwrap().save_lifecycle("run-1", &stale).unwrap();

    let loaded = Store::open(&db).unwrap().lifecycles_for_run("run-1").unwrap();
    assert_eq!(loaded, vec![stale]);
}

#[test]
fn saving_again_replaces_the_row_for_the_same_run_and_head() {
    let db = scratch("replace").join("orq.db");
    let store = Store::open(&db).unwrap();
    store.save_lifecycle("run-1", &associated()).unwrap();
    let merged = associated().apply(Ok(Observation { state: ProviderState::Merged, head_sha: HEAD.into() }));
    store.save_lifecycle("run-1", &merged).unwrap();

    let loaded = store.lifecycles_for_run("run-1").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].state, ProviderState::Merged);
    assert_eq!(loaded[0].gate, Gate::Resolved);
}

#[test]
fn a_recorded_provider_error_is_kept() {
    let db = scratch("error").join("orq.db");
    let store = Store::open(&db).unwrap();
    let failed = associated().apply(Err(ObservationError::PermissionDenied("403".into())));
    store.save_lifecycle("run-1", &failed).unwrap();
    assert_eq!(store.lifecycles_for_run("run-1").unwrap()[0].last_error, failed.last_error);
}

#[test]
fn lifecycles_are_kept_apart_per_run_and_an_unknown_run_has_none() {
    let db = scratch("runs").join("orq.db");
    let store = Store::open(&db).unwrap();
    store.save_lifecycle("run-1", &associated()).unwrap();
    assert!(store.lifecycles_for_run("run-2").unwrap().is_empty());
}

#[test]
fn identifiers_that_could_name_a_path_are_refused() {
    let db = scratch("ids").join("orq.db");
    let store = Store::open(&db).unwrap();
    assert!(store.save_lifecycle("../escape", &associated()).is_err());
    let bad_head = ReviewLifecycle::associate("github", "1", "u", "../x");
    assert!(store.save_lifecycle("run-1", &bad_head).is_err());
}

#[test]
fn a_database_from_before_lifecycles_is_upgraded_in_place() {
    let db = scratch("upgrade").join("orq.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE reviews (run_id TEXT NOT NULL, project TEXT NOT NULL, feature TEXT NOT NULL, head_sha TEXT NOT NULL, report_path TEXT NOT NULL, recorded_at INTEGER NOT NULL, PRIMARY KEY (run_id, head_sha));
             CREATE INDEX reviews_by_feature ON reviews (project, feature);
             CREATE INDEX reviews_by_head_sha ON reviews (head_sha);
             INSERT INTO reviews VALUES ('run-1','p','f','1111111111111111111111111111111111111111','runs/run-1/review/1111111111111111111111111111111111111111/review.json',0);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), Store::LATEST_VERSION);
    assert_eq!(store.reviews_for_run("run-1").unwrap().len(), 1);
    store.save_lifecycle("run-1", &associated()).unwrap();
}
