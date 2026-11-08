//! Each review row and report carries the time the report was generated, read from the stored report.

mod web_deps;

use orq_core::remote_inbox::{handle, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::{json, Value};

fn ask(roots: &StorageRoots, path: &str) -> Value {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle(roots, &config, "p", "p", &request);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

fn report(extra: Value) -> Vec<u8> {
    let mut base = json!({"status": "current", "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []});
    for (key, value) in extra.as_object().unwrap() {
        base[key] = value.clone();
    }
    serde_json::to_vec(&base).unwrap()
}

#[test]
fn the_list_and_the_detail_carry_when_the_report_was_generated_or_null_when_it_says_nothing_usable() {
    let base = web_deps::private_temp_dir("generated-at");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-a", "p", "fa", &"a".repeat(40), &report(json!({"generated_at": "2026-11-03T10:20:30Z"}))).unwrap();
    store.save_review(&roots.data, "run-b", "p", "fb", &"b".repeat(40), &report(json!({}))).unwrap();
    store.save_review(&roots.data, "run-c", "p", "fc", &"c".repeat(40), &report(json!({"generated_at": "<script>alert(1)</script>"}))).unwrap();

    let list = ask(&roots, "/api/reviews");
    let at = |run: &str| list["reviews"].as_array().unwrap().iter().find(|r| r["run_id"] == run).unwrap()["generated_at"].clone();
    assert_eq!(at("run-a"), "2026-11-03T10:20:30Z", "{list}");
    assert_eq!(at("run-b"), Value::Null, "{list}");
    assert_eq!(at("run-c"), Value::Null, "only a plain UTC time is passed on: {list}");
    assert_eq!(ask(&roots, "/api/reviews/run-a")["generated_at"], "2026-11-03T10:20:30Z");
    assert_eq!(ask(&roots, "/api/reviews/run-b")["generated_at"], Value::Null);
}

#[test]
fn a_report_that_cannot_be_read_as_a_report_and_an_older_head_give_null_not_another_reports_time() {
    let base = web_deps::private_temp_dir("generated-at-selection");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    // The same run reported at two heads: the older report's time must not stand in for the newer one's.
    store.save_review(&roots.data, "run-d", "p", "fd", &"1".repeat(40), &report(json!({"generated_at": "2026-01-01T00:00:00Z"}))).unwrap();
    store.save_review(&roots.data, "run-d", "p", "fd", &"2".repeat(40), &report(json!({}))).unwrap();
    // Reports that are not a report at all.
    store.save_review(&roots.data, "run-e", "p", "fe", &"e".repeat(40), b"{ not json").unwrap();
    store.save_review(&roots.data, "run-f", "p", "ff", &"f".repeat(40), b"[\"2026-11-03T10:20:30Z\"]").unwrap();

    let list = ask(&roots, "/api/reviews");
    for run in ["run-d", "run-e", "run-f"] {
        for row in list["reviews"].as_array().unwrap().iter().filter(|r| r["run_id"] == run) {
            assert_eq!(row["generated_at"], Value::Null, "{run}: {list}");
        }
    }
}
