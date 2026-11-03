use std::sync::mpsc;
use std::time::Duration;

use orq_core::review_inbox::{detail, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn a_report_replaced_by_a_fifo_is_refused_instead_of_blocking_the_reader() {
    let base = std::env::temp_dir().join(format!("orq-inbox-fifo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "COM-01", HEAD, br#"{"status":"incomplete","risks":[]}"#).unwrap();
    let report = roots.data.join(format!("runs/run-1/review/{HEAD}/review.json"));
    std::fs::remove_file(&report).unwrap();
    assert!(std::process::Command::new("mkfifo").arg(&report).status().unwrap().success());

    // The reader runs on its own thread so a hang fails the test instead of freezing it.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let rows = inbox(&roots, "p-0a1b2c3d", "p").unwrap();
        let err = detail(&roots, "run-1").unwrap_err();
        let _ = done.send((rows[0].status.clone(), err));
    });
    let (status, err) = finished.recv_timeout(Duration::from_secs(5)).expect("the reader blocked on a FIFO");
    assert_eq!(status, "unreadable");
    assert!(err.contains("unreadable"), "{err}");
}
