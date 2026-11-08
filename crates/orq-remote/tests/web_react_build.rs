//! The React page: its built bundle is served like the other assets, and the build is repeatable.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "web-react-test-token-0123456789abc";

fn start(tag: &str) -> Server {
    let base = std::env::temp_dir().join(format!("orq-remote-react-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap()
}

fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = raw.split_once("\r\n\r\n").expect("no header/body separator");
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

#[test]
fn the_react_bundle_is_served_without_a_token_with_the_same_safe_headers() {
    let server = start("serve");
    let (status, head, body) = get(server.addr(), "/app.react.js");
    assert_eq!(status, 200, "{head}");
    assert!(!body.is_empty());
    let lower = head.to_lowercase();
    assert!(lower.contains("content-type: text/javascript"), "{head}");
    assert!(lower.contains("cache-control: no-store"), "{head}");
    assert!(lower.contains("x-content-type-options: nosniff"), "{head}");
    assert!(lower.contains("script-src 'self'"), "{head}");
}
