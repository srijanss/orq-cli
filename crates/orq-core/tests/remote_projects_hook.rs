//! `/api/projects` goes through the same peer, token and method checks as every other path, and
//! only then asks the provider the server supplies. Without a provider the path does not exist.

use std::cell::Cell;
use std::net::IpAddr;

use orq_core::remote_inbox::{handle, handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Counting {
    calls: Cell<usize>,
    reply: Result<Value, String>,
}

impl ProjectsProvider for Counting {
    fn projects(&self) -> Result<Value, String> {
        self.calls.set(self.calls.get() + 1);
        self.reply.clone()
    }
}

fn roots() -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-projects-hook-{}", std::process::id()));
    StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") }
}

fn config() -> RemoteConfig {
    RemoteConfig { token: "tok".into() }
}

fn ask(provider: &Counting, method: &str, path: &str, auth: Option<&str>, peer: &str) -> (u16, Value) {
    let peer: IpAddr = peer.parse().unwrap();
    let r = handle_with(&roots(), &config(), "p-1", "p", &Request { method, path, peer, authorization: auth }, provider);
    (r.status, r.body)
}

fn provider() -> Counting {
    Counting { calls: Cell::new(0), reply: Ok(json!([{"id": "a"}])) }
}

#[test]
fn a_valid_get_returns_what_the_provider_gives_under_projects() {
    let p = provider();
    let (status, body) = ask(&p, "GET", "/api/projects", Some("Bearer tok"), "127.0.0.1");
    assert_eq!((status, body), (200, json!({"projects": [{"id": "a"}]})));
    assert_eq!(p.calls.get(), 1);
}

#[test]
fn a_refused_request_never_reaches_the_provider() {
    let p = provider();
    assert_eq!(ask(&p, "GET", "/api/projects", None, "127.0.0.1").0, 401);
    assert_eq!(ask(&p, "GET", "/api/projects", Some("Bearer wrong"), "127.0.0.1").0, 401);
    assert_eq!(ask(&p, "GET", "/api/projects", Some("Bearer tok"), "8.8.8.8").0, 401);
    assert_eq!(ask(&p, "POST", "/api/projects", Some("Bearer tok"), "127.0.0.1").0, 405);
    assert_eq!(ask(&p, "GET", "/api/projects/x", Some("Bearer tok"), "127.0.0.1").0, 404);
    assert_eq!(p.calls.get(), 0, "the provider must only be asked after every check passed");
}

#[test]
fn head_has_no_body_a_failing_provider_is_a_500_and_without_one_the_path_is_unknown() {
    let p = provider();
    let (status, body) = ask(&p, "HEAD", "/api/projects", Some("Bearer tok"), "127.0.0.1");
    assert!(status == 200 && body.is_null());
    let failing = Counting { calls: Cell::new(0), reply: Err("cannot read the projects".into()) };
    assert_eq!(ask(&failing, "GET", "/api/projects", Some("Bearer tok"), "127.0.0.1").0, 500);
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let plain = handle(&roots(), &config(), "p-1", "p", &Request { method: "GET", path: "/api/projects", peer, authorization: Some("Bearer tok") });
    assert_eq!(plain.status, 404);
}
