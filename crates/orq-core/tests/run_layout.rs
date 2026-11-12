use std::path::PathBuf;

use orq_core::storage::{save_run_file, RunFile};
use orq_core::store::Store;

const SHA: &str = "773a1660000000000000000000000000000000bb";

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-run-layout-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_saved_report_lands_under_its_head_sha_and_is_found_again_after_a_restart() {
    let root = scratch("review");
    let db = root.join("orq.db");
    let saved = {
        let store = Store::open(&db).unwrap();
        store.save_review(&root, "r1", "alpha", "login", SHA, b"{\"findings\":[]}").unwrap()
    };
    assert_eq!(saved.report_path, format!("runs/r1/review/{SHA}/review.json"));
    assert_eq!(std::fs::read(root.join(&saved.report_path)).unwrap(), b"{\"findings\":[]}");

    let store = Store::open(&db).unwrap(); // a restarted Orq
    let found = store.reviews_for_head_sha(SHA).unwrap();
    assert_eq!(found, vec![saved.clone()]);
    assert_eq!(std::fs::read(root.join(&found[0].report_path)).unwrap(), b"{\"findings\":[]}");

    // A later report for the same run and head SHA replaces the file and the row.
    store.save_review(&root, "r1", "alpha", "login", SHA, b"{\"findings\":[1]}").unwrap();
    assert_eq!(store.reviews_for_run("r1").unwrap().len(), 1);
    assert_eq!(std::fs::read(root.join(&saved.report_path)).unwrap(), b"{\"findings\":[1]}");
}

#[test]
fn unsafe_run_ids_and_head_shas_write_nothing() {
    let root = scratch("unsafe");
    let store = Store::open(&root.join("orq.db")).unwrap();
    assert!(store.save_review(&root, "../escape", "alpha", "login", SHA, b"x").is_err());
    assert!(store.save_review(&root, "r1", "alpha", "login", "../../escape", b"x").is_err());
    assert!(save_run_file(&root, "a/b", RunFile::Handoff, b"x").is_err());
    assert!(!root.join("runs").exists(), "a refused write must not even create directories");
    assert!(!root.parent().unwrap().join("escape").exists());
}

#[test]
fn a_report_that_cannot_be_written_is_not_indexed() {
    let root = scratch("write-fails");
    let store = Store::open(&root.join("orq.db")).unwrap();
    // The report's destination is already a directory, so the write must fail.
    std::fs::create_dir_all(root.join(format!("runs/r1/review/{SHA}/review.json"))).unwrap();
    assert!(store.save_review(&root, "r1", "alpha", "login", SHA, b"{}").is_err());
    assert!(store.reviews_for_run("r1").unwrap().is_empty(), "no index row may point at a report that was not written");
}

#[test]
fn an_index_failure_after_the_report_was_written_is_reported_and_leaves_a_retryable_state() {
    let root = scratch("index-fails");
    let db = root.join("orq.db");
    let store = Store::open(&db).unwrap();
    // Make the index INSERT fail after the report file has been written.
    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.execute_batch("CREATE TRIGGER refuse_index BEFORE INSERT ON reviews BEGIN SELECT RAISE(ABORT, 'index refused'); END;").unwrap();

    assert!(store.save_review(&root, "r1", "alpha", "login", SHA, b"{\"v\":1}").is_err());
    assert!(store.reviews_for_run("r1").unwrap().is_empty(), "no row may exist for the failed insert");
    let report = root.join(format!("runs/r1/review/{SHA}/review.json"));
    assert!(!report.exists(), "a report the index refused is rolled back, not left half-published");

    // Once the index accepts writes again, retrying the same save indexes it.
    raw.execute_batch("DROP TRIGGER refuse_index;").unwrap();
    store.save_review(&root, "r1", "alpha", "login", SHA, b"{\"v\":1}").unwrap();
    assert_eq!(store.reviews_for_run("r1").unwrap().len(), 1);
}

#[test]
fn secrets_never_reach_a_stored_report_or_run_file() {
    let root = scratch("redacted");
    let store = Store::open(&root.join("orq.db")).unwrap();
    let report = b"{\"findings\":[{\"note\":\"leaked gh\x70_abcdefghijklmnopqrstuvwxyz0123456789 and password=hunter2hunter2\"}]}";
    let saved = store.save_review(&root, "r1", "alpha", "login", SHA, report).unwrap();
    let stored = std::fs::read_to_string(root.join(&saved.report_path)).unwrap();
    assert!(!stored.contains("gh\x70_abcdefghijklmnopqrstuvwxyz0123456789") && !stored.contains("hunter2hunter2"), "{stored}");
    assert!(stored.contains("[REDACTED]") && stored.contains("\"findings\""));

    for file in [RunFile::Tests, RunFile::ReviewRounds, RunFile::Handoff, RunFile::Events] {
        save_run_file(&root, "r1", file, b"{\"log\":\"git clone https://u:pa55w0rd@example.com/r.git\"}\n").unwrap();
        let stored = std::fs::read_to_string(root.join("runs/r1").join(file.file_name())).unwrap();
        assert!(!stored.contains("pa55w0rd"), "{} kept a credential: {stored}", file.file_name());
    }
}

#[test]
fn run_files_are_written_only_under_their_fixed_names() {
    let root = scratch("files");
    for (file, name) in [
        (RunFile::Tests, "tests.json"),
        (RunFile::ReviewRounds, "review-rounds.json"),
        (RunFile::Handoff, "handoff.json"),
        (RunFile::Events, "events.jsonl"),
    ] {
        save_run_file(&root, "r1", file, name.as_bytes()).unwrap();
        assert_eq!(std::fs::read(root.join("runs/r1").join(name)).unwrap(), name.as_bytes());
    }
    let mut names: Vec<_> = std::fs::read_dir(root.join("runs/r1")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["events.jsonl", "handoff.json", "review-rounds.json", "tests.json"]);
}
