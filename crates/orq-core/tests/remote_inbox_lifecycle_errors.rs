use std::net::IpAddr;

use orq_core::remote_inbox::{handle, RemoteConfig, Request};
use orq_core::review_lifecycle::ReviewLifecycle;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OURS: &str = "p-0a1b2c3d";

fn seeded(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-lc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", OURS, "COM-01", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_lifecycle("run-1", &ReviewLifecycle::associate("github", "7", "https://github.com/o/r/pull/7", HEAD)).unwrap();
    roots
}

fn get(roots: &StorageRoots, path: &str) -> serde_json::Value {
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let config = RemoteConfig { token: "tok".into() };
    let r = handle(roots, &config, OURS, "p", &Request { method: "GET", path, peer, authorization: Some("Bearer tok") });
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

#[test]
fn an_unreadable_lifecycle_is_stale_never_fresh_in_the_detail_and_the_list_alike() {
    let roots = seeded("corrupt");
    assert_eq!(get(&roots, "/api/reviews/run-1")["stale"], false);
    let conn = rusqlite::Connection::open(roots.database()).unwrap();
    conn.execute("UPDATE review_lifecycles SET state = 'bogus'", []).unwrap();
    assert_eq!(get(&roots, "/api/reviews/run-1")["stale"], true, "the detail must not look fresh");
    assert_eq!(get(&roots, "/api/reviews")["reviews"][0]["stale"], true);
    assert!(get(&roots, "/api/reviews/run-1")["link"].is_null(), "a link of an unreadable lifecycle is not offered");
}
