use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "test-token-value";

fn state(tag: &str) -> Arc<ServerState> {
    let base = std::env::temp_dir().join(format!("orq-remote-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() })
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
    assert_eq!(reply.status, 403, "{}", reply.head);
}

#[test]
fn a_dropped_server_stops_listening() {
    let server = start("drop");
    let addr = server.addr();
    assert_eq!(request(addr, "GET", "/api/reviews", Some(TOKEN)).status, 200);
    drop(server);
    assert!(TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_err());
}
