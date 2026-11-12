use orq_core::review_lifecycle::{Gate, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, std::path::PathBuf, StorageRoots) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-journal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let db = roots.database();
    (Store::open(&db).unwrap(), db, roots)
}

fn lifecycle() -> ReviewLifecycle {
    ReviewLifecycle {
        provider: "github".into(),
        review_id: "7".into(),
        url: "https://example.test/7".into(),
        reported_head_sha: "a".repeat(40),
        observed_head_sha: None,
        state: ProviderState::Merged,
        gate: Gate::AwaitingHumanReview,
        stale: false,
        last_error: None,
        observed: true,
    }
}

#[test]
fn an_intended_removal_is_listed_until_it_is_finished() {
    let (store, _, _) = open("pending");
    assert!(store.pending_removals().unwrap().is_empty());
    store.begin_removal("run-b", "runs/run-b/events.jsonl").unwrap();
    store.begin_removal("run-a", "runs/run-a/events.jsonl").unwrap();
    store.begin_removal("run-a", "runs/run-a/events.jsonl").unwrap();
    assert_eq!(
        store.pending_removals().unwrap(),
        [("run-a".to_string(), "runs/run-a/events.jsonl".to_string()), ("run-b".to_string(), "runs/run-b/events.jsonl".to_string())]
    );
    store.finish_removal("runs/run-a/events.jsonl").unwrap();
    store.finish_removal("runs/run-a/events.jsonl").unwrap();
    assert_eq!(store.pending_removals().unwrap().len(), 1);
}

#[test]
fn an_unsafe_run_id_cannot_be_journaled() {
    let (store, _, _) = open("unsafe");
    assert!(store.begin_removal("../x", "runs/x/events.jsonl").is_err());
    assert!(store.pending_removals().unwrap().is_empty());
}

#[test]
fn finishing_a_removal_forgets_the_report_index_row_for_that_file() {
    let (store, _, roots) = open("index");
    let sha = "a".repeat(40);
    let other = "b".repeat(40);
    let kept = store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &other, br#"{"status":"current","risks":[]}"#).unwrap();
    let gone = store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &sha, br#"{"status":"current","risks":[]}"#).unwrap();
    store.begin_removal("run-1", &gone.report_path).unwrap();
    store.finish_removal(&gone.report_path).unwrap();
    let left: Vec<_> = store.reviews_for_run("run-1").unwrap().into_iter().map(|r| r.report_path).collect();
    assert_eq!(left, [kept.report_path]);
    assert!(store.pending_removals().unwrap().is_empty());
}

#[test]
fn removing_a_runs_metadata_leaves_other_runs_alone() {
    let (store, _, roots) = open("metadata");
    let sha = "a".repeat(40);
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &sha, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_review(&roots.data, "run-2", "p-0a1b2c3d", "F", &sha, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_lifecycle("run-1", &lifecycle()).unwrap();
    store.save_lifecycle("run-2", &lifecycle()).unwrap();
    store.remove_run_metadata("run-1").unwrap();
    store.remove_run_metadata("run-1").unwrap();
    assert!(store.reviews_for_run("run-1").unwrap().is_empty());
    assert!(store.lifecycles_for_run("run-1").unwrap().is_empty());
    assert_eq!(store.reviews_for_run("run-2").unwrap().len(), 1);
    assert_eq!(store.lifecycles_for_run("run-2").unwrap().len(), 1);
    assert!(store.remove_run_metadata("../x").is_err());
}

#[test]
fn a_version_three_database_is_upgraded_keeping_its_reviews() {
    let (store, db, roots) = open("upgrade");
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", &"a".repeat(40), br#"{"status":"current","risks":[]}"#).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings RENAME TO project_settings_old; ALTER TABLE prune_journal RENAME TO prune_journal_old; ALTER TABLE review_lifecycles DROP COLUMN observed;").unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    drop(conn);
    let store = Store::open(&db).unwrap();
    store.begin_removal("run-1", "runs/run-1/events.jsonl").unwrap();
    assert_eq!(store.all_reviews().unwrap().len(), 1);
    assert_eq!(store.pending_removals().unwrap().len(), 1);
}
