//! A software passkey for tests that go through the `/auth` routes: it enrols with the access token,
//! signs in, and answers any assertion challenge, the way a browser and authenticator would.
#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};

use orq_core::passkey_challenge::encode_b64url;
use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::{AuthReply, AuthRequest, PasskeyAuth};
use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TOKEN: &str = "passkey-support-test-token-0123456789";
pub const RP: &str = "box.tail1234.ts.net";
pub const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

/// What an answer says about itself; a test changes one part to forge it.
#[derive(Clone)]
pub struct Answer {
    pub origin: String,
    pub flags: u8,
    pub count: u32,
    pub flip_signature: bool,
}

impl Answer {
    pub fn good(count: u32) -> Answer {
        Answer { origin: format!("https://{RP}"), flags: 0x05, count, flip_signature: false }
    }
}

pub struct Device {
    signer: SigningKey,
    pub id: Vec<u8>,
}

impl Device {
    pub fn new(seed: u8) -> Device {
        Device { signer: SigningKey::from_bytes((&[seed; 32]).into()).unwrap(), id: vec![seed; 32] }
    }

    pub fn register_body(&self, challenge: &str) -> Vec<u8> {
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
        let client = json!({"type": "webauthn.create", "challenge": challenge, "origin": format!("https://{RP}"), "crossOrigin": false}).to_string();
        json!({"clientDataJSON": encode_b64url(client.as_bytes()), "attestationObject": encode_b64url(&attestation), "label": format!("device {}", self.id[0])}).to_string().into_bytes()
    }

    /// The assertion `navigator.credentials.get` would hand back for `challenge`, as the page sends it.
    pub fn assertion(&self, challenge: &str, answer: &Answer) -> Value {
        let mut auth = Sha256::digest(RP.as_bytes()).to_vec();
        auth.push(answer.flags);
        auth.extend(answer.count.to_be_bytes());
        let client = json!({"type": "webauthn.get", "challenge": challenge, "origin": answer.origin, "crossOrigin": false}).to_string();
        let mut signed = auth.clone();
        signed.extend(Sha256::digest(client.as_bytes()));
        let signature: Signature = self.signer.sign(&signed);
        let mut der = signature.to_der().as_bytes().to_vec();
        if answer.flip_signature {
            let last = der.len() - 1;
            der[last] ^= 0x01;
        }
        json!({
            "id": encode_b64url(&self.id),
            "clientDataJSON": encode_b64url(client.as_bytes()),
            "authenticatorData": encode_b64url(&auth),
            "signature": encode_b64url(&der),
        })
    }
}

pub struct Rig {
    pub auth: PasskeyAuth,
    pub roots: StorageRoots,
    pub config: RemoteConfig,
}

impl Rig {
    pub fn new(tag: &str) -> Rig {
        let base = std::env::temp_dir().join(format!("orq-core-passkey-support-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        roots.create_all().unwrap();
        Rig { auth: PasskeyAuth::new(PasskeyHost::from_setting(RP).unwrap()), roots, config: RemoteConfig { token: TOKEN.to_string() } }
    }

    pub fn call(&self, method: &str, path: &str, authorization: Option<&str>, cookie: Option<&str>, body: &[u8]) -> AuthReply {
        let request = AuthRequest { method, path, peer: LOCAL, authorization, cookie, body };
        self.auth.handle(&self.roots, &self.config, &request).expect("an auth route")
    }

    /// Enrols the device with the access token.
    pub fn enrol(&self, device: &Device) {
        let bearer = format!("Bearer {TOKEN}");
        let options = self.call("POST", "/auth/register/begin", Some(&bearer), None, b"{}").body;
        let reply = self.call("POST", "/auth/register/finish", Some(&bearer), None, &device.register_body(options["challenge"].as_str().unwrap()));
        assert_eq!(reply.status, 200, "{:?}", reply.body);
    }

    /// Signs the device in and returns the `Cookie` header the browser would then send.
    pub fn sign_in(&self, device: &Device, count: u32) -> String {
        let options = self.call("POST", "/auth/login/begin", None, None, b"{}").body;
        let body = device.assertion(options["challenge"].as_str().unwrap(), &Answer::good(count)).to_string();
        let reply = self.call("POST", "/auth/login/finish", None, None, body.as_bytes());
        assert_eq!(reply.status, 200, "{:?}", reply.body);
        reply.set_cookie.unwrap().split(';').next().unwrap().to_string()
    }
}
