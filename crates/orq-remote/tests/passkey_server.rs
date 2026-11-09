//! The passkey routes and the token policy, over a real socket.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::PasskeyAuth;
use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "passkey-server-test-token-0123456789";

struct NoProjects;
impl ProjectsProvider for NoProjects {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

fn start(tag: &str) -> (Server, StorageRoots) {
    let base = std::env::temp_dir().join(format!("orq-remote-passkey-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots: roots.clone(), config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let auth = Arc::new(PasskeyAuth::new(PasskeyHost::from_setting("box.tail1234.ts.net").unwrap()));
    let server = Server::start_with_auth(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(NoProjects), auth).unwrap();
    (server, roots)
}

fn register_a_key(roots: &StorageRoots) {
    let key = Passkey { credential_id: "AAAAAAAAAAAAAAAA".into(), public_key: "AAAA".into(), sign_count: 0, label: "phone".into(), created_at: 1 };
    PasskeyStore::open(roots).add_first(key).unwrap();
}

/// Status line, header block and body.
fn send(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (String, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.lines().next().unwrap_or("").to_string();
    (status, head.to_string(), body.to_string())
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

#[test]
fn status_says_whether_a_passkey_exists_and_that_nobody_is_signed_in() {
    let (server, roots) = start("status");
    let (status, _, body) = send(server.addr(), "GET", "/auth/status", &[], b"");
    assert!(status.contains("200"), "{status}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!({"passkeys": false, "signed_in": false, "host": "box.tail1234.ts.net"}));
    register_a_key(&roots);
    let (_, _, body) = send(server.addr(), "GET", "/auth/status", &[("Cookie", "__Host-orq-session=forged")], b"");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!((value["passkeys"].clone(), value["signed_in"].clone()), (true.into(), false.into()));
}

#[test]
fn the_token_enrols_the_first_passkey_but_not_without_it() {
    let (server, _roots) = start("begin");
    let (status, _, body) = send(server.addr(), "POST", "/auth/register/begin", &[], b"{}");
    assert!(status.contains("401"), "{status}");
    assert!(!body.contains("challenge"));
    let auth = bearer();
    let (status, _, body) = send(server.addr(), "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    assert!(status.contains("200"), "{status} {body}");
    assert!(body.contains("challenge") && body.contains("box.tail1234.ts.net"));
}

#[test]
fn once_a_passkey_exists_the_token_no_longer_enrols_or_opens_the_api_through_a_proxy() {
    let (server, roots) = start("policy");
    let auth = bearer();
    // Even before any passkey, a proxied token opens nothing but the first enrolment.
    let (status, _, _) = send(server.addr(), "GET", "/api/projects", &[("Authorization", &auth), ("X-Forwarded-For", "100.64.0.9")], b"");
    assert!(status.contains("401"), "{status}");
    register_a_key(&roots);
    // A proxied request (tailscale serve connects from loopback) no longer gets in with the token.
    for forwarded in [("X-Forwarded-For", "100.64.0.9"), ("Tailscale-User-Login", "someone@example.com")] {
        let (status, _, _) = send(server.addr(), "GET", "/api/projects", &[("Authorization", &auth), forwarded], b"");
        assert!(status.contains("401"), "{status}");
    }
    // A direct local connection keeps the token.
    let (status, _, _) = send(server.addr(), "GET", "/api/projects", &[("Authorization", &auth)], b"");
    assert!(status.contains("200"), "{status}");
    // The token cannot add a second passkey.
    let (status, _, _) = send(server.addr(), "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    assert!(status.contains("401"), "{status}");
    // A made-up session cookie opens nothing.
    let (status, _, _) = send(server.addr(), "GET", "/api/projects", &[("Cookie", "__Host-orq-session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"), ("X-Forwarded-For", "100.64.0.9")], b"");
    assert!(status.contains("401"), "{status}");
}

#[test]
fn sign_in_is_refused_without_a_passkey_and_for_garbage_with_one_and_sets_no_cookie() {
    let (server, roots) = start("login");
    let (status, _, _) = send(server.addr(), "POST", "/auth/login/begin", &[], b"{}");
    assert!(status.contains("401"), "{status}");
    register_a_key(&roots);
    let (status, _, body) = send(server.addr(), "POST", "/auth/login/begin", &[], b"{}");
    assert!(status.contains("200"), "{status}");
    assert!(body.contains("allowCredentials"));
    for garbage in [&b"{}"[..], b"not json", b"{\"id\":\"AAAA\",\"clientDataJSON\":\"AAAA\",\"authenticatorData\":\"AAAA\",\"signature\":\"AAAA\"}"] {
        let (status, head, _) = send(server.addr(), "POST", "/auth/login/finish", &[], garbage);
        assert!(status.contains("401"), "{status}");
        assert!(!head.to_lowercase().contains("set-cookie"), "{head}");
    }
}

#[test]
fn an_oversized_body_is_refused_and_unknown_auth_paths_are_not_found() {
    let (server, _roots) = start("limits");
    let big = vec![b'a'; 64 * 1024];
    let (status, _, _) = send(server.addr(), "POST", "/auth/login/finish", &[], &big);
    assert!(status.contains("413"), "{status}");
    let (status, _, _) = send(server.addr(), "GET", "/auth/nothing", &[], b"");
    assert!(status.contains("404"), "{status}");
}

#[test]
fn logout_always_clears_the_cookie() {
    let (server, _roots) = start("logout");
    let (status, head, _) = send(server.addr(), "POST", "/auth/logout", &[], b"{}");
    assert!(status.contains("200"), "{status}");
    assert!(head.to_lowercase().contains("set-cookie: __host-orq-session=;") && head.contains("Max-Age=0"), "{head}");
}
