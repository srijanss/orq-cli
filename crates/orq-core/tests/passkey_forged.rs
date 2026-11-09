//! Forged and replayed passkey requests, through the `/auth` routes: each is refused, no session
//! cookie is set and the store is left as it was.

use std::net::{IpAddr, Ipv4Addr};

use orq_core::passkey_challenge::{encode_b64url, Challenges};
use orq_core::passkey_login::{self, AssertionResponse};
use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::{AuthReply, AuthRequest, PasskeyAuth};
use orq_core::passkey_store::PasskeyStore;
use orq_core::remote_inbox::RemoteConfig;
use orq_core::session::Sessions;
use orq_core::storage::StorageRoots;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TOKEN: &str = "passkey-forged-test-token-0123456789";
const RP: &str = "box.tail1234.ts.net";
const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

/// What a forger changes about an otherwise good answer.
#[derive(Clone)]
struct Forge {
    origin: String,
    rp: String,
    flags: u8,
    count: u32,
    kind: &'static str,
    flip_signature: bool,
}

impl Forge {
    fn login() -> Forge {
        Forge { origin: format!("https://{RP}"), rp: RP.into(), flags: 0x05, count: 1, kind: "webauthn.get", flip_signature: false }
    }
    fn register() -> Forge {
        Forge { flags: 0x45, count: 0, kind: "webauthn.create", ..Forge::login() }
    }
}

struct Device {
    signer: SigningKey,
    id: Vec<u8>,
}

impl Device {
    fn new(seed: u8) -> Device {
        Device { signer: SigningKey::from_bytes((&[seed; 32]).into()).unwrap(), id: vec![seed; 32] }
    }

    fn client_data(forge: &Forge, options: &Value) -> String {
        json!({"type": forge.kind, "challenge": options["challenge"], "origin": forge.origin, "crossOrigin": false}).to_string()
    }

    fn register_body(&self, options: &Value, forge: &Forge) -> Vec<u8> {
        let point = self.signer.verifying_key().to_encoded_point(false);
        let cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(point.x().unwrap())), (int(-3), bytes(point.y().unwrap()))]);
        let mut auth = Sha256::digest(forge.rp.as_bytes()).to_vec();
        auth.push(forge.flags);
        auth.extend(forge.count.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.id.len() as u16).to_be_bytes());
        auth.extend(&self.id);
        auth.extend(cose);
        let attestation = map(vec![(text("fmt"), text("none")), (text("attStmt"), map(vec![])), (text("authData"), bytes(&auth))]);
        let client = Self::client_data(forge, options);
        json!({"clientDataJSON": encode_b64url(client.as_bytes()), "attestationObject": encode_b64url(&attestation), "label": "phone"}).to_string().into_bytes()
    }

    fn login_body(&self, id: &[u8], options: &Value, forge: &Forge) -> Vec<u8> {
        let mut auth = Sha256::digest(forge.rp.as_bytes()).to_vec();
        auth.push(forge.flags);
        auth.extend(forge.count.to_be_bytes());
        let client = Self::client_data(forge, options);
        let mut signed = auth.clone();
        signed.extend(Sha256::digest(client.as_bytes()));
        let signature: Signature = self.signer.sign(&signed);
        let mut der = signature.to_der().as_bytes().to_vec();
        if forge.flip_signature {
            let last = der.len() - 1;
            der[last] ^= 0x01;
        }
        json!({
            "id": encode_b64url(id),
            "clientDataJSON": encode_b64url(client.as_bytes()),
            "authenticatorData": encode_b64url(&auth),
            "signature": encode_b64url(&der),
        })
        .to_string()
        .into_bytes()
    }
}

struct Rig {
    auth: PasskeyAuth,
    roots: StorageRoots,
    config: RemoteConfig,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let base = std::env::temp_dir().join(format!("orq-core-forged-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        roots.create_all().unwrap();
        Rig { auth: PasskeyAuth::new(PasskeyHost::from_setting(RP).unwrap()), roots, config: RemoteConfig { token: TOKEN.to_string() } }
    }

    fn call(&self, method: &str, path: &str, authorization: Option<&str>, body: &[u8]) -> AuthReply {
        let request = AuthRequest { method, path, peer: LOCAL, authorization, cookie: None, body };
        self.auth.handle(&self.roots, &self.config, &request).expect("an auth route")
    }

    fn stored(&self) -> Vec<(String, u32)> {
        PasskeyStore::open(&self.roots).list().unwrap().into_iter().map(|k| (k.credential_id, k.sign_count)).collect()
    }

    fn begin_register(&self) -> Value {
        let bearer = format!("Bearer {TOKEN}");
        let reply = self.call("POST", "/auth/register/begin", Some(&bearer), b"{}");
        assert_eq!(reply.status, 200);
        reply.body
    }

    fn finish_register(&self, body: &[u8]) -> AuthReply {
        let bearer = format!("Bearer {TOKEN}");
        self.call("POST", "/auth/register/finish", Some(&bearer), body)
    }

    /// Enrols the device with a good answer.
    fn enrol(&self, device: &Device) {
        let options = self.begin_register();
        let reply = self.finish_register(&device.register_body(&options, &Forge::register()));
        assert_eq!(reply.status, 200, "{:?}", reply.body);
    }

    fn begin_login(&self) -> Value {
        let reply = self.call("POST", "/auth/login/begin", None, b"{}");
        assert_eq!(reply.status, 200);
        reply.body
    }

    fn finish_login(&self, body: &[u8]) -> AuthReply {
        self.call("POST", "/auth/login/finish", None, body)
    }

    fn refused_login(&self, reply: &AuthReply) {
        assert_eq!(reply.status, 401, "{:?}", reply.body);
        assert!(reply.set_cookie.is_none());
    }
}

#[test]
fn forged_enrolments_store_no_passkey() {
    let device = Device::new(7);
    let cases: Vec<(&str, Forge)> = vec![
        ("wrong origin", Forge { origin: "https://evil.example".into(), ..Forge::register() }),
        ("wrong relying party", Forge { rp: "evil.example".into(), ..Forge::register() }),
        ("no user verification", Forge { flags: 0x41, ..Forge::register() }),
        ("a sign-in answer", Forge { kind: "webauthn.get", ..Forge::register() }),
    ];
    for (name, forge) in cases {
        let rig = Rig::new("enrol");
        let options = rig.begin_register();
        let reply = rig.finish_register(&device.register_body(&options, &forge));
        assert_eq!(reply.status, 400, "{name}: {:?}", reply.body);
        assert!(reply.set_cookie.is_none(), "{name}");
        assert!(rig.stored().is_empty(), "{name}");
    }
}

#[test]
fn a_replayed_enrolment_answer_adds_nothing_and_a_wrong_challenge_is_refused() {
    let device = Device::new(7);
    let rig = Rig::new("replay-enrol");
    let options = rig.begin_register();
    let body = device.register_body(&options, &Forge::register());
    assert_eq!(rig.finish_register(&body).status, 200);
    let before = rig.stored();
    assert_eq!(before.len(), 1);
    // The same answer again, with the token still in hand, must not be accepted.
    assert!(rig.finish_register(&body).status >= 400);
    assert_eq!(rig.stored(), before);

    let rig = Rig::new("wrong-challenge");
    let _ = rig.begin_register();
    let invented = json!({"challenge": encode_b64url(&[9u8; 32])});
    let reply = rig.finish_register(&device.register_body(&invented, &Forge::register()));
    assert_eq!(reply.status, 400);
    assert!(rig.stored().is_empty());
}

#[test]
fn the_token_cannot_enrol_again_once_a_passkey_exists() {
    let rig = Rig::new("reused-code");
    let device = Device::new(7);
    rig.enrol(&device);
    let bearer = format!("Bearer {TOKEN}");
    let reply = rig.call("POST", "/auth/register/begin", Some(&bearer), b"{}");
    assert_eq!(reply.status, 401);
    let other = Device::new(8);
    let reply = rig.finish_register(&other.register_body(&json!({"challenge": encode_b64url(&[1u8; 32])}), &Forge::register()));
    assert_eq!(reply.status, 401);
    assert_eq!(rig.stored().len(), 1);
}

#[test]
fn forged_sign_ins_get_no_session_and_leave_the_counter_alone() {
    let device = Device::new(7);
    let cases: Vec<(&str, Forge, Vec<u8>)> = vec![
        ("wrong origin", Forge { origin: "https://evil.example".into(), ..Forge::login() }, device.id.clone()),
        ("wrong relying party", Forge { rp: "evil.example".into(), ..Forge::login() }, device.id.clone()),
        ("bad signature", Forge { flip_signature: true, ..Forge::login() }, device.id.clone()),
        ("no user verification", Forge { flags: 0x01, ..Forge::login() }, device.id.clone()),
        ("an enrolment answer", Forge { kind: "webauthn.create", ..Forge::login() }, device.id.clone()),
        ("unknown credential", Forge::login(), vec![99u8; 32]),
    ];
    for (name, forge, id) in cases {
        let rig = Rig::new("login");
        rig.enrol(&device);
        let before = rig.stored();
        let options = rig.begin_login();
        let reply = rig.finish_login(&device.login_body(&id, &options, &forge));
        assert_eq!(reply.status, 401, "{name}: {:?}", reply.body);
        assert!(reply.set_cookie.is_none(), "{name}");
        assert_eq!(rig.stored(), before, "{name}");
    }
}

#[test]
fn a_good_sign_in_cannot_be_replayed_and_a_counter_cannot_go_backwards() {
    let rig = Rig::new("replay-login");
    let device = Device::new(7);
    rig.enrol(&device);

    let options = rig.begin_login();
    let body = device.login_body(&device.id, &options, &Forge { count: 5, ..Forge::login() });
    let good = rig.finish_login(&body);
    assert_eq!(good.status, 200, "{:?}", good.body);
    assert!(good.set_cookie.is_some());
    assert_eq!(rig.stored()[0].1, 5);

    // The very same answer a second time: the challenge is spent.
    rig.refused_login(&rig.finish_login(&body));

    // A fresh challenge, but a counter that is not above the stored one.
    for count in [5, 4, 1] {
        let options = rig.begin_login();
        let reply = rig.finish_login(&device.login_body(&device.id, &options, &Forge { count, ..Forge::login() }));
        rig.refused_login(&reply);
        assert_eq!(rig.stored()[0].1, 5);
    }

    // And a counter that does move is still welcome.
    let options = rig.begin_login();
    let reply = rig.finish_login(&device.login_body(&device.id, &options, &Forge { count: 6, ..Forge::login() }));
    assert_eq!(reply.status, 200);
    assert_eq!(rig.stored()[0].1, 6);
}

#[test]
fn an_expired_challenge_is_refused() {
    let rig = Rig::new("expired");
    let device = Device::new(7);
    rig.enrol(&device);
    let store = PasskeyStore::open(&rig.roots);
    let host = PasskeyHost::from_setting(RP).unwrap();
    let challenges = Challenges::new();
    let sessions = Sessions::new(60);
    let options = passkey_login::begin(&host, &challenges, &store, 1_000).unwrap();
    let body: Value = serde_json::from_slice(&device.login_body(&device.id, &options, &Forge::login())).unwrap();
    let field = |key: &str| body[key].as_str().unwrap().to_string();
    let response = AssertionResponse { credential_id: field("id"), client_data_json: field("clientDataJSON"), authenticator_data: field("authenticatorData"), signature: field("signature") };
    // A day later the same, otherwise good, answer is no use.
    assert!(passkey_login::finish(&host, &challenges, &store, &sessions, &response, 1_000 + 24 * 60 * 60).is_err());
    assert_eq!(sessions.count(), 0);
    assert_eq!(rig.stored()[0].1, 0);
}
