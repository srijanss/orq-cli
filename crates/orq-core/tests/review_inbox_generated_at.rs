//! The generation time comes from the same report read that fills the rest of a row or a detail.

use orq_core::review_inbox::{detail_at, inbox};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::json;

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-genat-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn report(at: Option<&str>) -> Vec<u8> {
    let mut body = json!({"status": "current", "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []});
    if let Some(at) = at {
        body["generated_at"] = json!(at);
    }
    serde_json::to_vec(&body).unwrap()
}

#[test]
fn a_row_and_a_detail_take_the_time_of_the_report_for_their_own_head() {
    let roots = roots("select");
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p", "f", HEAD_A, &report(Some("2026-11-01T01:00:00Z"))).unwrap();
    store.save_review(&roots.data, "run-1", "p", "f", HEAD_B, &report(Some("2026-11-02T02:00:00Z"))).unwrap();
    let rows = inbox(&roots, "p", "p").unwrap();
    assert_eq!(rows[0].generated_at.as_deref(), Some("2026-11-02T02:00:00Z"), "the row shows the newest report");
    assert_eq!(detail_at(&roots, "run-1", HEAD_A).unwrap().generated_at.as_deref(), Some("2026-11-01T01:00:00Z"));
    assert_eq!(detail_at(&roots, "run-1", HEAD_B).unwrap().generated_at.as_deref(), Some("2026-11-02T02:00:00Z"));
}

#[test]
fn a_newer_report_without_a_time_never_falls_back_to_an_older_time() {
    let roots = roots("fallback");
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p", "f", HEAD_A, &report(Some("2026-11-01T01:00:00Z"))).unwrap();
    store.save_review(&roots.data, "run-1", "p", "f", HEAD_B, &report(None)).unwrap();
    assert_eq!(inbox(&roots, "p", "p").unwrap()[0].generated_at, None);
    assert_eq!(detail_at(&roots, "run-1", HEAD_B).unwrap().generated_at, None);
}

#[test]
fn an_odd_time_is_dropped() {
    let roots = roots("odd");
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p", "f", HEAD_A, &report(Some("<b>x</b>"))).unwrap();
    assert_eq!(inbox(&roots, "p", "p").unwrap()[0].generated_at, None);
}
