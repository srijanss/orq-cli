use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn seeded(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-beneath-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"incomplete","risks":[]}"#).unwrap();
    roots
}

#[test]
fn a_report_reached_through_a_symlinked_directory_is_not_read_even_inside_the_data_root() {
    let roots = seeded("ancestor");
    let run = roots.data.join("runs/run-1");
    std::fs::rename(run.join("review"), run.join("real")).unwrap();
    std::os::unix::fs::symlink(run.join("real"), run.join("review")).unwrap();

    let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!(rows[0].status, "unreadable", "{rows:?}");
    assert!(detail(&roots, "run-1").is_err());
}

#[test]
fn an_ordinary_report_is_still_read() {
    let roots = seeded("plain");
    let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
    assert_eq!(rows[0].status, "draft", "{rows:?}");
}
