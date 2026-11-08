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
