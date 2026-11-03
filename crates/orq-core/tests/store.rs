use std::path::PathBuf;

use orq_core::store::{ReviewRecord, Store};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(run: &str, project: &str, feature: &str, sha: &str) -> ReviewRecord {
    ReviewRecord {
        run_id: run.into(),
        project: project.into(),
        feature: feature.into(),
        head_sha: sha.into(),
        report_path: format!("runs/{run}/review/{sha}/review.json"),
    }
}

const SHA_A: &str = "49485ef0000000000000000000000000000000aa";
const SHA_B: &str = "773a1660000000000000000000000000000000bb";

#[test]
fn reports_are_queryable_by_run_project_feature_and_head_sha_and_survive_a_restart() {
    let db = scratch("restart").join("orq.db");
    {
        let store = Store::open(&db).unwrap();
        assert_eq!(store.schema_version().unwrap(), Store::LATEST_VERSION);
        store.record_review(&record("r1", "alpha", "login", SHA_A)).unwrap();
        store.record_review(&record("r1", "alpha", "login", SHA_B)).unwrap();
        store.record_review(&record("r2", "beta", "search", SHA_A)).unwrap();
    }
    let store = Store::open(&db).unwrap(); // a restarted Orq
    assert_eq!(store.reviews_for_run("r1").unwrap().len(), 2);
    assert_eq!(store.reviews_for_project("beta").unwrap().len(), 1);
    assert_eq!(store.reviews_for_feature("alpha", "login").unwrap().len(), 2);
    let by_sha = store.reviews_for_head_sha(SHA_A).unwrap();
    assert_eq!(by_sha.len(), 2);
    assert!(by_sha.iter().any(|r| r.run_id == "r2" && r.project == "beta"));

    // Re-recording the same run and head SHA replaces the row instead of duplicating it.
    store.record_review(&ReviewRecord { report_path: "runs/r1/review/new.json".into(), ..record("r1", "alpha", "login", SHA_A) }).unwrap();
    let again = store.reviews_for_run("r1").unwrap();
    assert_eq!(again.len(), 2);
    assert!(again.iter().any(|r| r.head_sha == SHA_A && r.report_path == "runs/r1/review/new.json"));
}

#[cfg(unix)]
#[test]
fn the_database_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let db = scratch("perm").join("orq.db");
    let store = Store::open(&db).unwrap();
    store.record_review(&record("r1", "alpha", "login", SHA_A)).unwrap();
    drop(store);
    for name in ["orq.db", "orq.db-wal", "orq.db-shm"] {
        let path = db.with_file_name(name);
        if let Ok(meta) = std::fs::metadata(&path) {
            assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{name}");
        }
    }
}

#[test]
fn identifiers_that_could_name_a_path_outside_the_run_are_refused() {
    let store = Store::open(&scratch("ids").join("orq.db")).unwrap();
    for run in ["", "../x", "a/b", ".", ".."] {
        assert!(store.record_review(&record(run, "alpha", "login", SHA_A)).is_err(), "run {run:?}");
    }
    for sha in ["", "../../x", "not-hex!", "ABC/def", "zz"] {
        assert!(store.record_review(&record("r1", "alpha", "login", sha)).is_err(), "sha {sha:?}");
    }
    assert!(store.reviews_for_run("r1").unwrap().is_empty());
}

#[test]
fn concurrent_writers_lose_no_rows() {
    let db = scratch("concurrent").join("orq.db");
    Store::open(&db).unwrap();
    std::thread::scope(|scope| {
        for writer in 0..8 {
            let db = &db;
            scope.spawn(move || {
                let store = Store::open(db).unwrap();
                for n in 0..25 {
                    let sha = format!("{:040x}", writer * 1000 + n);
                    store.record_review(&record(&format!("run{writer}"), "alpha", "login", &sha)).unwrap();
                }
            });
        }
    });
    let store = Store::open(&db).unwrap();
    assert_eq!(store.reviews_for_project("alpha").unwrap().len(), 200);
}

#[test]
fn an_interrupted_write_leaves_no_partial_state_and_a_newer_schema_is_refused() {
    let db = scratch("crash").join("orq.db");
    let store = Store::open(&db).unwrap();
    store.record_review(&record("r1", "alpha", "login", SHA_A)).unwrap();
    // A writer that dies mid-transaction: its uncommitted row must vanish on reopen.
    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.execute_batch(&format!(
        "BEGIN IMMEDIATE; INSERT INTO reviews (run_id, project, feature, head_sha, report_path, recorded_at) \
         VALUES ('r2', 'alpha', 'login', '{SHA_B}', 'x', 0);"
    ))
    .unwrap();
    drop(raw); // dropped without COMMIT, like a killed process
    drop(store);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.reviews_for_project("alpha").unwrap().len(), 1);
    drop(store);

    // A database written by a newer Orq is not opened (and not downgraded).
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", Store::LATEST_VERSION + 1).unwrap();
    drop(conn);
    assert!(Store::open(&db).is_err());
}
