use std::net::IpAddr;

use orq_core::remote_inbox::{handle, peer_allowed, safe_link, RemoteConfig, Request};
use orq_core::review_lifecycle::{Observation, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::json;

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OURS: &str = "orq-cli-0a1b2c3d";
const TOKEN: &str = "s3cret-token-value";

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-remote-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn seed(roots: &StorageRoots, url: &str) {
    let store = Store::open(&roots.database()).unwrap();
    let body = serde_json::to_vec(&json!({
        "status": "current",
        "summary": {"files_changed": 1, "added": 2, "deleted": 1},
        "files": [{"path": "src/a.rs", "added": 2, "deleted": 1}],
        "tests": {"status": "passed", "commands": []},
        "risks": [{"severity": "high", "file": "src/a.rs", "line": 4, "claim": "token logged"}],
        "rounds": [], "gaps": []
    }))
    .unwrap();
    store.save_review(&roots.data, "run-1", OURS, "COM-01", HEAD_A, &body).unwrap();
    store.save_lifecycle("run-1", &ReviewLifecycle::associate("github", "7", url, HEAD_A)).unwrap();
}

fn cfg() -> RemoteConfig {
    RemoteConfig { token: TOKEN.into() }
}

fn lan() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

fn get(roots: &StorageRoots, path: &str, peer: IpAddr, auth: Option<&str>) -> (u16, serde_json::Value) {
    request(roots, "GET", path, peer, auth)
}

fn request(roots: &StorageRoots, method: &str, path: &str, peer: IpAddr, auth: Option<&str>) -> (u16, serde_json::Value) {
    let r = handle(roots, &cfg(), OURS, "orq-cli", &Request { method, path, peer, authorization: auth });
    (r.status, r.body)
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

#[test]
fn an_authorised_peer_lists_reviews_and_reads_one_report() {
    let roots = roots("ok");
    seed(&roots, "https://github.com/o/r/pull/7");
    let auth = bearer();
    let (status, body) = get(&roots, "/api/reviews", lan(), Some(&auth));
    assert_eq!(status, 200, "{body}");
    let row = &body["reviews"][0];
    assert_eq!((row["run_id"].as_str(), row["feature"].as_str(), row["risk"].as_str()), (Some("run-1"), Some("COM-01"), Some("high")));
    assert_eq!(row["link"], "https://github.com/o/r/pull/7");

    let (status, body) = get(&roots, "/api/reviews/run-1", lan(), Some(&auth));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["reported_head_sha"], HEAD_A);
    assert!(body["risks"][0].as_str().unwrap().contains("token logged"));
    assert_eq!(body["link"], "https://github.com/o/r/pull/7");
}

#[test]
fn a_missing_or_wrong_token_is_refused_and_an_unset_token_refuses_everyone() {
    let roots = roots("auth");
    seed(&roots, "https://github.com/o/r/pull/7");
    for auth in [None, Some("Bearer nope"), Some("Basic s3cret-token-value"), Some("Bearer "), Some(TOKEN)] {
        assert_eq!(get(&roots, "/api/reviews", lan(), auth).0, 401, "{auth:?}");
    }
    let r = handle(&roots, &RemoteConfig { token: String::new() }, OURS, "orq-cli", &Request { method: "GET", path: "/api/reviews", peer: lan(), authorization: Some("Bearer ") });
    assert_eq!(r.status, 401);
}

#[test]
fn only_loopback_and_tailscale_peers_are_allowed() {
    for ok in ["127.0.0.1", "::1", "100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
        assert!(peer_allowed(ok.parse().unwrap()), "{ok}");
    }
    for bad in ["8.8.8.8", "192.168.1.5", "10.0.0.1", "100.63.255.255", "100.128.0.1", "2001:db8::1", "fd7a:115c:a1e1::1"] {
        assert!(!peer_allowed(bad.parse().unwrap()), "{bad}");
    }
    let roots = roots("peer");
    seed(&roots, "https://github.com/o/r/pull/7");
    let auth = bearer();
    assert_eq!(get(&roots, "/api/reviews", "8.8.8.8".parse().unwrap(), Some(&auth)).0, 403, "a good token does not open the door to a stranger");
}

#[test]
fn nothing_but_reading_is_possible() {
    let roots = roots("readonly");
    seed(&roots, "https://github.com/o/r/pull/7");
    let auth = bearer();
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for path in ["/api/reviews", "/api/reviews/run-1", "/api/reviews/run-1/merge"] {
            assert_eq!(request(&roots, method, path, lan(), Some(&auth)).0, 405, "{method} {path}");
        }
    }
    assert_eq!(get(&roots, "/api/reviews/run-1/merge", lan(), Some(&auth)).0, 404);
}

#[test]
fn a_changed_provider_head_shows_as_stale_on_the_next_request() {
    let roots = roots("stale");
    seed(&roots, "https://github.com/o/r/pull/7");
    let auth = bearer();
    assert_eq!(get(&roots, "/api/reviews/run-1", lan(), Some(&auth)).1["stale"], false);
    let store = Store::open(&roots.database()).unwrap();
    let moved = ReviewLifecycle::associate("github", "7", "https://github.com/o/r/pull/7", HEAD_A).apply(Ok(Observation { state: ProviderState::Open, head_sha: HEAD_B.into() }));
    store.save_lifecycle("run-1", &moved).unwrap();
    let (_, body) = get(&roots, "/api/reviews/run-1", lan(), Some(&auth));
    assert_eq!((body["stale"].clone(), body["observed_head_sha"].clone()), (json!(true), json!(HEAD_B)));
    assert_eq!(get(&roots, "/api/reviews", lan(), Some(&auth)).1["reviews"][0]["stale"], true);
}

#[test]
fn unknown_and_unsafe_run_ids_are_not_found_or_rejected() {
    let roots = roots("ids");
    seed(&roots, "https://github.com/o/r/pull/7");
    let auth = bearer();
    assert_eq!(get(&roots, "/api/reviews/nope", lan(), Some(&auth)).0, 404);
    for bad in ["/api/reviews/..", "/api/reviews/a%2F..%2Fb", "/api/reviews/a/b/c", "/api/reviews/"] {
        let status = get(&roots, bad, lan(), Some(&auth)).0;
        assert!(status == 400 || status == 404, "{bad}: {status}");
    }
}

#[test]
fn insecure_links_are_dropped_not_passed_on() {
    for bad in [
        "javascript:alert(1)", "http://github.com/o/r/pull/7", "data:text/html,x", "//evil.test/x", "https://user:pw@github.com/x",
        "https://github.com/x\r\nSet-Cookie: a=b", "https://github.com/a b", "https:///nohost", "ftp://x", "", "https://github.com/\u{202e}x",
        "https://github.com/\"onmouseover=1",
    ] {
        assert_eq!(safe_link(bad), None, "{bad:?}");
    }
    assert_eq!(safe_link("https://gitlab.com/g/p/-/merge_requests/3").as_deref(), Some("https://gitlab.com/g/p/-/merge_requests/3"));

    let roots = roots("link");
    seed(&roots, "javascript:alert(1)");
    let auth = bearer();
    assert!(get(&roots, "/api/reviews/run-1", lan(), Some(&auth)).1["link"].is_null());
    assert!(get(&roots, "/api/reviews", lan(), Some(&auth)).1["reviews"][0]["link"].is_null());
}
