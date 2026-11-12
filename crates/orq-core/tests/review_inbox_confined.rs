use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OUTSIDE: &str = r#"{"status":"current","risks":[{"severity":"high","claim":"outside"}]}"#;

fn seeded(tag: &str) -> (StorageRoots, std::path::PathBuf) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-inbox-conf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"incomplete","risks":[]}"#).unwrap();
    let outside = base.join("outside.json");
    std::fs::write(&outside, OUTSIDE).unwrap();
    (roots, outside)
}

fn point_index_at(roots: &StorageRoots, path: &str) {
    let conn = rusqlite::Connection::open(roots.database()).unwrap();
    conn.execute("UPDATE reviews SET report_path = ?1", [path]).unwrap();
}

fn assert_refused(roots: &StorageRoots) {
    let rows = inbox(roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!((rows[0].status.as_str(), rows[0].risk.as_str()), ("unreadable", "-"), "{rows:?}");
    let err = detail(roots, "run-1").unwrap_err();
    assert!(!err.contains("outside"), "{err}");
}

#[test]
fn an_absolute_path_in_the_index_is_not_read() {
    let (roots, outside) = seeded("abs");
    point_index_at(&roots, outside.to_str().unwrap());
    assert_refused(&roots);
}

#[test]
fn a_parent_relative_path_in_the_index_is_not_read() {
    let (roots, _) = seeded("dots");
    point_index_at(&roots, "runs/run-1/../../../outside.json");
    assert_refused(&roots);
}

#[test]
fn a_report_replaced_by_a_symlink_is_not_followed() {
    let (roots, outside) = seeded("link");
    let report = roots.data.join(format!("runs/run-1/review/{HEAD}/review.json"));
    std::fs::remove_file(&report).unwrap();
    std::os::unix::fs::symlink(&outside, &report).unwrap();
    assert_refused(&roots);
}
