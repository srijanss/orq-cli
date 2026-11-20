//! A signed-in passkey session for a test: a software authenticator enrols with the server's token,
//! signs in, and the session's cookie and CSRF token come back.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

use orq_core::passkey_challenge::encode_b64url;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use sha2::{Digest, Sha256};

pub const RP: &str = "box.tail1234.ts.net";

/// A session: what a signed-in page sends with each request.
pub struct Session {
    pub cookie: String,
    pub csrf: String,
}

pub fn raw(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15))).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0), head.to_string(), body.to_string())
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

fn client_data(kind: &str, options: &serde_json::Value) -> String {
    serde_json::json!({"type": kind, "challenge": options["challenge"], "origin": format!("https://{RP}"), "crossOrigin": false}).to_string()
}

/// Enrols a passkey with `token`, signs in with it, and returns the session.
pub fn sign_in(addr: SocketAddr, token: &str) -> Session {
    let signer = SigningKey::from_bytes((&[11u8; 32]).into()).unwrap();
    let id = vec![11u8; 32];
    let auth = format!("Bearer {token}");
    let json = |body: &str| -> serde_json::Value { serde_json::from_str(body).unwrap() };

    let (_, _, body) = raw(addr, "POST", "/auth/register/begin", &[("Authorization", &auth)], b"{}");
    let options = json(&body);
    let point = signer.verifying_key().to_encoded_point(false);
    let cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(point.x().unwrap())), (int(-3), bytes(point.y().unwrap()))]);
    let mut data = Sha256::digest(RP.as_bytes()).to_vec();
    data.push(0x45);
    data.extend(0u32.to_be_bytes());
    data.extend([0u8; 16]);
    data.extend((id.len() as u16).to_be_bytes());
    data.extend(&id);
    data.extend(cose);
    let attestation = map(vec![(text("fmt"), text("none")), (text("attStmt"), map(vec![])), (text("authData"), bytes(&data))]);
    let register = serde_json::json!({"clientDataJSON": encode_b64url(client_data("webauthn.create", &options).as_bytes()), "attestationObject": encode_b64url(&attestation), "label": "phone"});
    assert_eq!(raw(addr, "POST", "/auth/register/finish", &[("Authorization", &auth)], register.to_string().as_bytes()).0, 200);

    let (_, _, body) = raw(addr, "POST", "/auth/login/begin", &[], b"{}");
    let options = json(&body);
    let mut data = Sha256::digest(RP.as_bytes()).to_vec();
    data.push(0x05);
    data.extend(1u32.to_be_bytes());
    let client = client_data("webauthn.get", &options);
    let mut signed = data.clone();
    signed.extend(Sha256::digest(client.as_bytes()));
    let signature: Signature = signer.sign(&signed);
    let login = serde_json::json!({"id": encode_b64url(&id), "clientDataJSON": encode_b64url(client.as_bytes()), "authenticatorData": encode_b64url(&data), "signature": encode_b64url(signature.to_der().as_bytes())});
    let (status, head, body) = raw(addr, "POST", "/auth/login/finish", &[], login.to_string().as_bytes());
    assert_eq!(status, 200, "{body}");
    let line = head.lines().find(|l| l.to_lowercase().starts_with("set-cookie:")).expect("a session cookie");
    let cookie = line.split_once(':').unwrap().1.trim().split(';').next().unwrap().to_string();
    let (_, _, status) = raw(addr, "GET", "/auth/status", &[("Cookie", &cookie)], b"");
    let csrf = json(&status)["csrf"].as_str().expect("a CSRF token").to_string();
    Session { cookie, csrf }
}
