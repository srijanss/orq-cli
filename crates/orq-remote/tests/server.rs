use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "test-token-value";

fn state(tag: &str) -> Arc<ServerState> {
    state_with_roots(tag).0
}

fn state_with_roots(tag: &str) -> (Arc<ServerState>, StorageRoots) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots: roots.clone(), config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    (state, roots)
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn request(addr: SocketAddr, method: &str, path: &str, token: Option<&str>) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}Content-Length: 0\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    Reply { status, head: head.to_string(), body: body.to_string() }
}

fn start(tag: &str) -> Server {
    Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state(tag)).unwrap()
}

#[test]
fn an_authorised_get_of_the_inbox_is_served_as_json() {
    let server = start("get");
    let reply = request(server.addr(), "GET", "/api/reviews", Some(TOKEN));
    assert_eq!(reply.status, 200, "{}", reply.head);
    assert!(reply.head.to_lowercase().contains("content-type: application/json"), "{}", reply.head);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert!(value["reviews"].is_array(), "{value}");
}

#[test]
fn anything_but_get_or_head_is_refused_and_head_has_no_body() {
    let server = start("methods");
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        assert_eq!(request(server.addr(), method, "/api/reviews", Some(TOKEN)).status, 405, "{method}");
    }
    let head = request(server.addr(), "HEAD", "/api/reviews", Some(TOKEN));
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty(), "{}", head.body);
}

#[test]
fn a_missing_or_wrong_token_and_unknown_paths_are_refused() {
    let server = start("refusals");
    assert_eq!(request(server.addr(), "GET", "/api/reviews", None).status, 401);
    assert_eq!(request(server.addr(), "GET", "/api/reviews", Some("wrong")).status, 401);
    assert_eq!(request(server.addr(), "GET", "/nowhere", Some(TOKEN)).status, 404);
}

/// An address of this machine that is not loopback or Tailscale, if there is one.
fn outside_address() -> Option<IpAddr> {
    let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect("192.0.2.1:9").ok()?;
    let ip = probe.local_addr().ok()?.ip();
    (!orq_core::remote_inbox::peer_allowed(ip)).then_some(ip)
}

#[test]
fn a_connection_from_an_address_that_may_not_connect_is_refused() {
    let Some(ip) = outside_address() else {
        eprintln!("skipped: this machine has no address outside loopback and Tailscale");
        return;
    };
    let server = Server::start(TcpListener::bind("0.0.0.0:0").unwrap(), state("peer")).unwrap();
    let reply = request(SocketAddr::new(ip, server.addr().port()), "GET", "/api/reviews", Some(TOKEN));
    assert_eq!(reply.status, 401, "{}", reply.head);
}

#[test]
fn a_dropped_server_stops_listening() {
    let server = start("drop");
    let addr = server.addr();
    assert_eq!(request(addr, "GET", "/api/reviews", Some(TOKEN)).status, 200);
    drop(server);
    assert!(TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_err());
}

const HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn a_stored_report_is_listed_and_served_in_detail_with_safe_headers() {
    let (state, roots) = state_with_roots("detail");
    let store = orq_core::store::Store::open(&roots.database()).unwrap();
    let report = serde_json::json!({"status": "current", "risks": [{"severity": "high", "claim": "check it", "link": "https://example.invalid/a#L1"}]});
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "APP-01", HEAD_SHA, report.to_string().as_bytes()).unwrap();
    let server = Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap();

    let list = request(server.addr(), "GET", "/api/reviews", Some(TOKEN));
    assert_eq!(list.status, 200, "{}", list.head);
    let list: serde_json::Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(list["reviews"][0]["run_id"], "run-1", "{list}");

    let detail = request(server.addr(), "GET", "/api/reviews/run-1", Some(TOKEN));
    assert_eq!(detail.status, 200, "{}", detail.head);
    let lower = detail.head.to_lowercase();
    assert!(lower.contains("cache-control: no-store") && lower.contains("x-content-type-options: nosniff"), "{}", detail.head);
    let value: serde_json::Value = serde_json::from_str(&detail.body).unwrap();
    assert_eq!(value["reported_head_sha"], HEAD_SHA, "{value}");
    assert_eq!(value["run_id"], "run-1");
}

#[test]
fn bad_and_unknown_run_ids_are_refused_with_safe_headers_and_head_stays_body_free() {
    let server = start("ids");
    for (path, status) in [("/api/reviews/..%2Fsecret", 400), ("/api/reviews/no-such-run", 404), ("/api/reviews/a/b", 404)] {
        let reply = request(server.addr(), "GET", path, Some(TOKEN));
        assert_eq!(reply.status, status, "{path}: {}", reply.head);
        let lower = reply.head.to_lowercase();
        assert!(lower.contains("cache-control: no-store") && lower.contains("x-content-type-options: nosniff"), "{path}: {}", reply.head);
        let head = request(server.addr(), "HEAD", path, Some(TOKEN));
        assert_eq!(head.status, status, "{path}");
        assert!(head.body.is_empty(), "{path}: {}", head.body);
    }
}

#[test]
fn an_unreadable_store_is_reported_as_an_error_not_a_crash() {
    let (state, roots) = state_with_roots("badstore");
    std::fs::write(roots.database(), b"this is not a database").unwrap();
    let server = Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap();
    let reply = request(server.addr(), "GET", "/api/reviews", Some(TOKEN));
    assert!(reply.status == 500 || reply.status == 200, "{}", reply.head);
    assert!(serde_json::from_str::<serde_json::Value>(&reply.body).is_ok(), "{}", reply.body);
    assert_eq!(request(server.addr(), "GET", "/api/reviews/run-1", Some(TOKEN)).status, 404);
}

#[test]
fn dropping_the_server_is_not_held_up_by_a_client_that_never_finishes_its_request() {
    let server = start("stuck");
    let mut stuck = TcpStream::connect(server.addr()).unwrap();
    stuck.write_all(b"GET /api/reviews HTTP/1.1\r\nHost: x\r\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(server);
        let _ = done.send(());
    });
    assert!(finished.recv_timeout(std::time::Duration::from_secs(10)).is_ok(), "dropping the server hung on an unfinished request");
    drop(stuck);
}

#[test]
fn a_query_string_on_a_list_reaches_the_handler_over_http() {
    let server = start("query");
    let bad = request(server.addr(), "GET", "/api/reviews?limit=0", Some(TOKEN));
    assert_eq!(bad.status, 400, "{}", bad.body);
    assert_eq!(request(server.addr(), "GET", "/api/reviews?bogus=1", Some(TOKEN)).status, 400);
    let page = request(server.addr(), "GET", "/api/reviews?limit=1&sort=oldest", Some(TOKEN));
    assert_eq!(page.status, 200, "{}", page.body);
    let value: serde_json::Value = serde_json::from_str(&page.body).unwrap();
    assert!(value["total"].is_number() && value["next_cursor"].is_null(), "{value}");
}
