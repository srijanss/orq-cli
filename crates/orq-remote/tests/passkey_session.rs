//! A whole enrolment and sign-in over the socket, with a software authenticator.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::passkey_challenge::encode_b64url;
use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::PasskeyAuth;
use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use sha2::{Digest, Sha256};

const TOKEN: &str = "passkey-session-test-token-0123456789";
const RP: &str = "box.tail1234.ts.net";

/// One run: its first look has event e1, and once the flag is raised the next look has e2.
struct NoProjects;
static GROW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
impl ProjectsProvider for NoProjects {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
    fn run_timeline(&self, _id: &str, _run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        let events = match after {
            None => vec![serde_json::json!({"event_id": "e1"})],
            Some("e1") if GROW.load(std::sync::atomic::Ordering::SeqCst) => vec![serde_json::json!({"event_id": "e2"})],
            Some(_) => vec![],
        };
        Ok(Some(serde_json::json!({"run": {"run_id": "r1"}, "events": events, "next_cursor": null})))
    }
}

fn start(tag: &str) -> (Server, StorageRoots) {
    start_with(tag, true)
}

fn start_with(tag: &str, loopback_token: bool) -> (Server, StorageRoots) {
    let base = std::env::temp_dir().join(format!("orq-remote-session-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots: roots.clone(), config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let auth = Arc::new(PasskeyAuth::new(PasskeyHost::from_setting(RP).unwrap()).with_loopback_token(loopback_token));
    let server = Server::start_with_auth(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(NoProjects), auth).unwrap();
    (server, roots)
}

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
    (head.lines().next().unwrap_or("").to_string(), head.to_string(), body.to_string())
}

fn cbor_head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        _ => vec![m | 25, (n >> 8) as u8, n as u8],
    }
}
fn int(n: i64) -> Vec<u8> {
    if n >= 0 { cbor_head(0, n as u64) } else { cbor_head(1, (-1 - n) as u64) }
}
fn bytes(b: &[u8]) -> Vec<u8> {
    [cbor_head(2, b.len() as u64), b.to_vec()].concat()
}
fn text(t: &str) -> Vec<u8> {
    [cbor_head(3, t.len() as u64), t.as_bytes().to_vec()].concat()
}
fn map(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut out = cbor_head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(k);
        out.extend(v);
    }
    out
}

struct Device {
    signer: SigningKey,
    id: Vec<u8>,
    count: u32,
}

impl Device {
    fn new(seed: u8) -> Device {
        Device { signer: SigningKey::from_bytes((&[seed; 32]).into()).unwrap(), id: vec![seed; 32], count: 0 }
    }

    fn client_data(kind: &str, options: &serde_json::Value) -> String {
        serde_json::json!({"type": kind, "challenge": options["challenge"], "origin": format!("https://{RP}"), "crossOrigin": false}).to_string()
    }

    fn register_body(&self, options: &serde_json::Value, label: &str) -> Vec<u8> {
        let point = self.signer.verifying_key().to_encoded_point(false);
        let cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(point.x().unwrap())), (int(-3), bytes(point.y().unwrap()))]);
        let mut auth = Sha256::digest(RP.as_bytes()).to_vec();
        auth.push(0x45);
        auth.extend(0u32.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.id.len() as u16).to_be_bytes());
        auth.extend(&self.id);
        auth.extend(cose);
        let attestation = map(vec![(text("fmt"), text("none")), (text("attStmt"), map(vec![])), (text("authData"), bytes(&auth))]);
        let client = Self::client_data("webauthn.create", options);
        serde_json::json!({"clientDataJSON": encode_b64url(client.as_bytes()), "attestationObject": encode_b64url(&attestation), "label": label}).to_string().into_bytes()
    }

    fn login_body(&mut self, options: &serde_json::Value) -> Vec<u8> {
        self.count += 1;
        let mut auth = Sha256::digest(RP.as_bytes()).to_vec();
        auth.push(0x05);
        auth.extend(self.count.to_be_bytes());
        let client = Self::client_data("webauthn.get", options);
        let mut signed = auth.clone();
        signed.extend(Sha256::digest(client.as_bytes()));
        let signature: Signature = self.signer.sign(&signed);
        serde_json::json!({
            "id": encode_b64url(&self.id),
            "clientDataJSON": encode_b64url(client.as_bytes()),
            "authenticatorData": encode_b64url(&auth),
            "signature": encode_b64url(signature.to_der().as_bytes()),
        })
        .to_string()
        .into_bytes()
    }
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not json: {body}"))
}

fn cookie_from(head: &str) -> String {
    let line = head.lines().find(|l| l.to_lowercase().starts_with("set-cookie:")).expect("no cookie set");
    line.split_once(':').unwrap().1.trim().split(';').next().unwrap().to_string()
}

#[test]
fn a_passkey_is_enrolled_with_the_token_then_signs_in_and_the_session_works_until_logout() {
    let (server, _roots) = start("whole");
    let addr = server.addr();
    let auth = format!("Bearer {TOKEN}");
    let mut phone = Device::new(11);

    let (_, _, body) = send(addr, "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    let (status, _, _) = send(addr, "POST", "/auth/register/finish", &[("Authorization", &auth)], &phone.register_body(&json(&body), "phone"));
    assert!(status.contains("200"), "{status}");

    let (_, _, body) = send(addr, "POST", "/auth/login/begin", &[], b"{}");
    let (status, head, body) = send(addr, "POST", "/auth/login/finish", &[], &phone.login_body(&json(&body)));
    assert!(status.contains("200"), "{status} {body}");
    let cookie = cookie_from(&head);
    assert!(head.contains("HttpOnly") && head.contains("Secure"), "{head}");

    // Through a proxy: the session opens the API, the token does not.
    let proxied = [("Cookie", cookie.as_str()), ("X-Forwarded-For", "100.64.0.9")];
    assert!(send(addr, "GET", "/api/projects", &proxied, b"").0.contains("200"));
    assert_eq!(json(&send(addr, "GET", "/auth/status", &proxied, b"").2)["signed_in"], true);
    assert!(send(addr, "GET", "/api/projects", &[("Authorization", &auth), ("X-Forwarded-For", "100.64.0.9")], b"").0.contains("401"));

    // The session adds a second passkey; the token alone could not.
    let tablet = Device::new(22);
    let (status, _, body) = send(addr, "POST", "/auth/register/begin", &proxied, b"{}");
    assert!(status.contains("200"), "{status}");
    let (status, _, _) = send(addr, "POST", "/auth/register/finish", &proxied, &tablet.register_body(&json(&body), "tablet"));
    assert!(status.contains("200"), "{status}");
    assert!(send(addr, "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}").0.contains("401"));

    // A replayed assertion is refused: the challenge is single-use.
    let (_, _, options) = send(addr, "POST", "/auth/login/begin", &[], b"{}");
    let once = phone.login_body(&json(&options));
    assert!(send(addr, "POST", "/auth/login/finish", &[], &once).0.contains("200"));
    assert!(send(addr, "POST", "/auth/login/finish", &[], &once).0.contains("401"));

    // Logout ends the session on the server, for plain requests and for streams alike.
    assert!(send(addr, "POST", "/auth/logout", &proxied, b"{}").0.contains("200"));
    assert!(send(addr, "GET", "/api/projects", &proxied, b"").0.contains("401"));
    assert!(send(addr, "GET", "/api/projects/p-0a1b2c3d/runs/r1/events", &proxied, b"").0.contains("401"));
}

#[test]
fn an_unreadable_passkey_store_refuses_the_token_through_a_proxy() {
    let (server, roots) = start("corrupt");
    let key = Passkey { credential_id: "AAAAAAAAAAAAAAAA".into(), public_key: "AAAA".into(), sign_count: 0, label: "phone".into(), created_at: 1 };
    PasskeyStore::open(&roots).add_first(key).unwrap();
    std::fs::write(PasskeyStore::path(&roots), b"{ not json").unwrap();
    let auth = format!("Bearer {TOKEN}");
    let (status, _, _) = send(server.addr(), "GET", "/api/projects", &[("Authorization", &auth), ("X-Forwarded-For", "100.64.0.9")], b"");
    assert!(status.contains("401"), "{status}");
}

/// Reads a stream until it ends or `limit` passes; whether it ended, and everything it said.
fn drain(stream: &mut TcpStream, limit: std::time::Duration) -> (bool, String) {
    let start = std::time::Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while start.elapsed() < limit {
        match stream.read(&mut buf) {
            Ok(0) => return (true, String::from_utf8_lossy(&got).to_string()),
            Ok(n) => got.extend(&buf[..n]),
            Err(_) => {}
        }
        // The body's last chunk ends the stream even though keep-alive holds the socket.
        if got.ends_with(b"0\r\n\r\n") {
            return (true, String::from_utf8_lossy(&got).to_string());
        }
    }
    (false, String::from_utf8_lossy(&got).to_string())
}

#[test]
fn a_stream_already_open_stops_when_its_session_is_ended_and_never_sees_the_next_event() {
    let (server, _roots) = start("stream");
    let addr = server.addr();
    let auth = format!("Bearer {TOKEN}");
    let mut phone = Device::new(33);
    let (_, _, body) = send(addr, "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    assert!(send(addr, "POST", "/auth/register/finish", &[("Authorization", &auth)], &phone.register_body(&json(&body), "phone")).0.contains("200"));
    let (_, _, body) = send(addr, "POST", "/auth/login/begin", &[], b"{}");
    let (_, head, _) = send(addr, "POST", "/auth/login/finish", &[], &phone.login_body(&json(&body)));
    let cookie = cookie_from(&head);

    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
    write!(stream, "GET /api/projects/p1/runs/r1/events HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 100.64.0.9\r\nCookie: {cookie}\r\n\r\n").unwrap();
    let (ended, first) = drain(&mut stream, std::time::Duration::from_millis(1500));
    assert!(!ended && first.contains("200") && first.contains("e1"), "{first}");

    // Sign out, then let the run grow: the open connection must close without the new event.
    let proxied = [("Cookie", cookie.as_str()), ("X-Forwarded-For", "100.64.0.9")];
    assert!(send(addr, "POST", "/auth/logout", &proxied, b"{}").0.contains("200"));
    GROW.store(true, std::sync::atomic::Ordering::SeqCst);
    let (ended, rest) = drain(&mut stream, std::time::Duration::from_secs(6));
    assert!(ended, "the stream stayed open: {rest}");
    assert!(!rest.contains("e2"), "{rest}");
}

#[test]
fn with_the_token_switched_off_a_session_is_the_only_way_in_and_logout_and_forgery_close_it() {
    let (server, _roots) = start_with("passkey-only", false);
    let addr = server.addr();
    let auth = format!("Bearer {TOKEN}");
    let mut phone = Device::new(44);
    let (_, _, body) = send(addr, "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    assert!(send(addr, "POST", "/auth/register/finish", &[("Authorization", &auth)], &phone.register_body(&json(&body), "phone")).0.contains("200"));
    let (_, _, body) = send(addr, "POST", "/auth/login/begin", &[], b"{}");
    let (_, head, _) = send(addr, "POST", "/auth/login/finish", &[], &phone.login_body(&json(&body)));
    let cookie = cookie_from(&head);
    let session = [("Cookie", cookie.as_str())];
    let forged = [("Cookie", "__Host-orq-session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")];
    let stream_status = |headers: &[(&str, &str)]| {
        let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
        write!(stream, "GET /api/projects/p1/runs/r1/events HTTP/1.1\r\nHost: x\r\n{extra}\r\n").unwrap();
        drain(&mut stream, std::time::Duration::from_millis(1000)).1
    };

    // Direct local connection: the token is refused, the session is not.
    assert!(send(addr, "GET", "/api/projects", &[("Authorization", &auth)], b"").0.contains("401"));
    assert!(send(addr, "GET", "/api/projects", &session, b"").0.contains("200"));
    assert!(stream_status(&session).contains("200"));
    assert!(send(addr, "GET", "/api/projects", &forged, b"").0.contains("401"));
    assert!(stream_status(&forged).contains("401"));
    assert!(stream_status(&[("Authorization", &auth)]).contains("401"));

    // Once signed out the same cookie opens nothing.
    assert!(send(addr, "POST", "/auth/logout", &session, b"{}").0.contains("200"));
    assert!(send(addr, "GET", "/api/projects", &session, b"").0.contains("401"));
    assert!(stream_status(&session).contains("401"));
}
