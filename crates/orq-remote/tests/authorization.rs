use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{refusal_line, Server, ServerState};

const TOKEN: &str = "authorization-test-token-0123456789";

fn start(tag: &str) -> Server {
    let base = std::env::temp_dir().join(format!("orq-remote-auth-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap()
}

/// The status line, the headers (minus the per-response Date) and the body.
fn raw(addr: SocketAddr, method: &str, path: &str, auth: Option<&str>) -> (String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let auth = auth.map(|a| format!("Authorization: {a}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}Content-Length: 0\r\n\r\n").unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("the response was cut short or the read failed");
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text.split_once("\r\n\r\n").expect("the response has no header/body separator");
    if method != "HEAD" {
        let length = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).expect("no content-length");
        assert_eq!(body.len(), length, "the body is not the advertised length");
    }
    let head: Vec<&str> = head.lines().filter(|line| !line.to_lowercase().starts_with("date:")).collect();
    (head.join("\n"), body.to_string())
}

#[test]
fn every_kind_of_refusal_looks_the_same_on_the_wire() {
    let server = start("same");
    let addr = server.addr();
    let good = format!("Bearer {TOKEN}");
    let wrong = format!("Bearer {}", "x".repeat(TOKEN.len()));
    let reference = raw(addr, "GET", "/api/reviews", None);
    assert!(reference.0.starts_with("HTTP/1.1 401"), "{}", reference.0);
    assert!(!reference.1.contains(TOKEN) && !reference.0.contains(TOKEN));
    let refused = [
        raw(addr, "GET", "/api/reviews", Some(&wrong)),
        raw(addr, "GET", "/api/reviews", Some("Bearer short")),
        raw(addr, "GET", "/api/reviews", Some("Basic abc")),
        raw(addr, "GET", "/api/reviews", Some(TOKEN)),
        raw(addr, "GET", "/nowhere", None),
        raw(addr, "GET", "/nowhere", Some(&wrong)),
        raw(addr, "GET", "/api/reviews/run-1", None),
        raw(addr, "POST", "/api/reviews", None),
        raw(addr, "DELETE", "/api/reviews/run-1", Some(&wrong)),
    ];
    for (i, got) in refused.iter().enumerate() {
        assert_eq!(got, &reference, "refusal #{i} is distinguishable");
    }
    // The same paths with the right token are answered differently, so the
    // comparison above is not vacuous.
    assert!(raw(addr, "GET", "/nowhere", Some(&good)).0.starts_with("HTTP/1.1 404"));
    assert!(raw(addr, "GET", "/api/reviews", Some(&good)).0.starts_with("HTTP/1.1 200"));
}

#[test]
fn head_refusals_match_each_other_and_carry_no_body() {
    let server = start("head");
    let addr = server.addr();
    let wrong = format!("Bearer {}", "x".repeat(TOKEN.len()));
    let reference = raw(addr, "HEAD", "/api/reviews", None);
    assert!(reference.0.starts_with("HTTP/1.1 401"), "{}", reference.0);
    assert!(reference.1.is_empty(), "{}", reference.1);
    let refused = [
        raw(addr, "HEAD", "/api/reviews", Some(&wrong)),
        raw(addr, "HEAD", "/api/reviews", Some("Bearer short")),
        raw(addr, "HEAD", "/api/reviews", Some("Basic abc")),
        raw(addr, "HEAD", "/api/reviews", Some(TOKEN)),
        raw(addr, "HEAD", "/nowhere", None),
        raw(addr, "HEAD", "/nowhere", Some(&wrong)),
        raw(addr, "HEAD", "/api/reviews/run-1", None),
    ];
    for (i, got) in refused.iter().enumerate() {
        assert_eq!(got, &reference, "HEAD refusal #{i} is distinguishable");
    }
    // A HEAD refusal has the status of the GET refusal.
    let get = raw(addr, "GET", "/api/reviews", None);
    assert_eq!(reference.0.lines().next(), get.0.lines().next());
}

#[test]
fn the_refusal_log_line_names_the_peer_and_request_but_never_a_credential() {
    let peer = "100.64.0.9".parse().unwrap();
    let line = refusal_line(peer, "GET", "/api/reviews");
    assert!(line.contains("100.64.0.9") && line.contains("GET") && line.contains("/api/reviews"), "{line}");
    assert!(!line.to_lowercase().contains("authorization") && !line.contains("Bearer"), "{line}");
    assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");

    // A hostile path cannot forge extra log lines and is bounded.
    let hostile = refusal_line(peer, "GET", &format!("/a\norq-remote: accepted\r{}", "z".repeat(5000)));
    assert!(!hostile.contains('\n') && !hostile.contains('\r'), "{hostile:?}");
    assert!(hostile.len() < 400, "{}", hostile.len());
}
