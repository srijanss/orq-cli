use std::net::IpAddr;

use orq_core::remote_inbox::{handle, RemoteConfig, Request};
use orq_core::review_lifecycle::ReviewLifecycle;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OURS: &str = "p-0a1b2c3d";

fn seeded(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-head-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", OURS, "COM-01", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
    store.save_lifecycle("run-1", &ReviewLifecycle::associate("github", "7", "https://github.com/o/r/pull/7", HEAD)).unwrap();
    roots
}

fn call(roots: &StorageRoots, method: &str, path: &str, auth: Option<&str>) -> (u16, serde_json::Value) {
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let r = handle(roots, &RemoteConfig { token: "tok".into() }, OURS, "p", &Request { method, path, peer, authorization: auth });
    (r.status, r.body)
}

#[test]
fn head_answers_like_get_but_carries_no_body() {
    let roots = seeded("body");
    for path in ["/api/reviews", "/api/reviews/run-1"] {
        let (get_status, get_body) = call(&roots, "GET", path, Some("Bearer tok"));
        let (head_status, head_body) = call(&roots, "HEAD", path, Some("Bearer tok"));
        assert_eq!((get_status, head_status), (200, 200), "{path}");
        assert!(get_body.is_object());
        assert!(head_body.is_null(), "{path}: HEAD must not carry a body");
    }
}

#[test]
fn head_is_held_to_the_same_checks_as_get() {
    let roots = seeded("checks");
    assert_eq!(call(&roots, "HEAD", "/api/reviews", None).0, 401);
    assert_eq!(call(&roots, "HEAD", "/api/reviews/nope", Some("Bearer tok")).0, 404);
    assert_eq!(call(&roots, "HEAD", "/api/other", Some("Bearer tok")).0, 404);
}

#[test]
fn on_the_wire_a_head_response_has_zero_body_bytes_and_a_get_has_json() {
    let roots = seeded("wire");
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let ask = |method: &str, path: &str, auth: Option<&str>| {
        handle(&roots, &RemoteConfig { token: "tok".into() }, OURS, "p", &Request { method, path, peer, authorization: auth })
    };
    for (path, auth) in [("/api/reviews", Some("Bearer tok")), ("/api/reviews", None), ("/api/reviews/nope", Some("Bearer tok"))] {
        assert!(ask("HEAD", path, auth).body_bytes().is_empty(), "{path} {auth:?}");
        assert!(serde_json::from_slice::<serde_json::Value>(&ask("GET", path, auth).body_bytes()).is_ok());
    }
}
