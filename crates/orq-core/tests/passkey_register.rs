//! Registering a passkey: the options carry a fresh single-use challenge, and the browser's answer
//! is stored only when every check holds.

use orq_core::passkey_challenge::{encode_b64url, Challenges};
use orq_core::passkey_enroll::{EnrolAuthorizer, Enrolment};
use orq_core::passkey_register::{begin, finish, PasskeyHost, RegistrationResponse};
use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::storage::StorageRoots;
use sha2::{Digest, Sha256};

const HOST: &str = "box.tailnet.ts.net";

struct Fixture {
    host: PasskeyHost,
    challenges: Challenges,
    authorizer: EnrolAuthorizer,
    store: PasskeyStore,
}

fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("orq-register-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    Fixture { host: PasskeyHost::from_setting(HOST).unwrap(), challenges: Challenges::new(), authorizer: EnrolAuthorizer::new(), store: PasskeyStore::open(&roots) }
}

// A just-enough CBOR encoder for building answers.
fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        _ => vec![m | 25, (n >> 8) as u8, n as u8],
    }
}
fn int(n: i64) -> Vec<u8> {
    if n >= 0 { head(0, n as u64) } else { head(1, (-1 - n) as u64) }
}
fn bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}
fn text(t: &str) -> Vec<u8> {
    [head(3, t.len() as u64), t.as_bytes().to_vec()].concat()
}
fn map(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut out = head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(k);
        out.extend(v);
    }
    out
}

fn cose_key(x: &[u8], y: &[u8]) -> Vec<u8> {
    map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(x)), (int(-3), bytes(y))])
}

struct Answer {
    rp_id: String,
    flags: u8,
    sign_count: u32,
    credential_id: Vec<u8>,
    cose: Vec<u8>,
    extra: Vec<u8>,
    client_type: String,
    origin: String,
    cross_origin: Option<bool>,
    fmt: String,
}

impl Answer {
    fn good() -> Answer {
        Answer { rp_id: HOST.into(), flags: 0x45, sign_count: 0, credential_id: vec![7; 32], cose: cose_key(&[1; 32], &[2; 32]), extra: vec![], client_type: "webauthn.create".into(), origin: format!("https://{HOST}"), cross_origin: Some(false), fmt: "none".into() }
    }

    fn response(&self, challenge: &str) -> RegistrationResponse {
        let mut auth = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        auth.push(self.flags);
        auth.extend(self.sign_count.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.credential_id.len() as u16).to_be_bytes());
        auth.extend(&self.credential_id);
        auth.extend(&self.cose);
        auth.extend(&self.extra);
        let attestation = map(vec![(text("fmt"), text(&self.fmt)), (text("attStmt"), map(vec![])), (text("authData"), bytes(&auth))]);
        let mut client = serde_json::json!({"type": self.client_type, "challenge": challenge, "origin": self.origin});
        if let Some(cross) = self.cross_origin {
            client["crossOrigin"] = cross.into();
        }
        RegistrationResponse { client_data_json: encode_b64url(client.to_string().as_bytes()), attestation_object: encode_b64url(&attestation), label: "laptop".into() }
    }
}

fn challenge_of(options: &serde_json::Value) -> String {
    options["challenge"].as_str().unwrap().to_string()
}

fn try_register(f: &Fixture, answer: &Answer, now: u64) -> Result<Passkey, String> {
    let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &answer.response(&challenge_of(&options)), now)
}

#[test]
fn the_host_setting_must_be_a_plain_https_host_name() {
    let ok = PasskeyHost::from_setting(HOST).unwrap();
    assert_eq!((ok.rp_id(), ok.origin()), (HOST, "https://box.tailnet.ts.net"));
    for bad in ["", "localhost", "https://box.tailnet.ts.net", "box.tailnet.ts.net:8443", "box.tailnet.ts.net/path", "100.101.102.103", "Box.Tailnet.ts.net", "a..b", "-a.b.c", "a b.c.d", "box.tailnet.ts.net."] {
        assert!(PasskeyHost::from_setting(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn the_options_name_the_configured_host_ask_for_es256_and_no_attestation_and_use_a_new_challenge_each_time() {
    let f = fixture("options");
    let one = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    let two = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    assert_eq!(one["rp"]["id"], HOST);
    assert_eq!(one["attestation"], "none");
    assert_eq!(one["pubKeyCredParams"][0]["alg"], -7);
    assert_eq!(one["authenticatorSelection"]["userVerification"], "required");
    assert_ne!(one["challenge"], two["challenge"]);
    assert_ne!(one["user"]["id"], two["user"]["id"]);
    assert_eq!(one["excludeCredentials"].as_array().unwrap().len(), 0);
}

#[test]
fn existing_passkeys_are_excluded_from_new_registrations() {
    let f = fixture("exclude");
    f.store.add(Passkey { credential_id: "abc".into(), public_key: "AQID".into(), sign_count: 0, label: "x".into(), created_at: 1 }).unwrap();
    let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    assert_eq!(options["excludeCredentials"][0]["id"], "abc");
}

#[test]
fn a_good_answer_stores_the_key_with_its_id_count_and_label() {
    let f = fixture("good");
    let mut answer = Answer::good();
    answer.sign_count = 5;
    let stored = try_register(&f, &answer, 120).unwrap();
    assert_eq!(stored.credential_id, encode_b64url(&[7; 32]));
    assert_eq!((stored.sign_count, stored.label.as_str()), (5, "laptop"));
    assert_eq!(f.store.list().unwrap(), vec![stored.clone()]);
    assert_eq!(stored.public_key, encode_b64url(&cose_key(&[1; 32], &[2; 32])));
}

#[test]
fn an_answer_with_extension_data_is_accepted_when_the_flag_says_so() {
    let f = fixture("ext");
    let mut answer = Answer::good();
    answer.flags |= 0x80;
    answer.extra = map(vec![(text("credProps"), map(vec![]))]);
    assert!(try_register(&f, &answer, 120).is_ok());
}

#[test]
fn every_check_refuses_and_stores_nothing() {
    let cases: Vec<(&str, Box<dyn Fn(&mut Answer)>)> = vec![
        ("wrong type", Box::new(|a| a.client_type = "webauthn.get".into())),
        ("wrong origin", Box::new(|a| a.origin = "https://evil.example".into())),
        ("origin with port", Box::new(|a| a.origin = format!("https://{HOST}:8443"))),
        ("http origin", Box::new(|a| a.origin = format!("http://{HOST}"))),
        ("cross origin", Box::new(|a| a.cross_origin = Some(true))),
        ("wrong rp id hash", Box::new(|a| a.rp_id = "other.tailnet.ts.net".into())),
        ("no user presence", Box::new(|a| a.flags = 0x44)),
        ("no user verification", Box::new(|a| a.flags = 0x41)),
        ("no credential data", Box::new(|a| a.flags = 0x05)),
        ("empty credential id", Box::new(|a| a.credential_id = vec![])),
        ("oversized credential id", Box::new(|a| a.credential_id = vec![1; 769])),
        ("wrong algorithm", Box::new(|a| a.cose = map(vec![(int(1), int(2)), (int(3), int(-257)), (int(-1), int(1)), (int(-2), bytes(&[1; 32])), (int(-3), bytes(&[2; 32]))]))),
        ("wrong curve", Box::new(|a| a.cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(2)), (int(-2), bytes(&[1; 32])), (int(-3), bytes(&[2; 32]))]))),
        ("short coordinate", Box::new(|a| a.cose = cose_key(&[1; 31], &[2; 32]))),
        ("extra key entry", Box::new(|a| a.cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(&[1; 32])), (int(-3), bytes(&[2; 32])), (int(9), int(9))]))),
        ("trailing bytes", Box::new(|a| a.extra = vec![0])),
        ("extension flag without data", Box::new(|a| a.flags |= 0x80)),
    ];
    for (name, change) in cases {
        let f = fixture("refuse");
        let mut answer = Answer::good();
        change(&mut answer);
        assert!(try_register(&f, &answer, 120).is_err(), "{name} should be refused");
        assert!(f.store.list().unwrap().is_empty(), "{name} stored a key");
    }
}

#[test]
fn a_challenge_works_once_and_expires() {
    let f = fixture("once");
    let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    let response = Answer::good().response(&challenge_of(&options));
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithSession, &response, 120).is_ok());
    // Replaying the same answer fails, even though the key is not yet a duplicate of anything new.
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithSession, &response, 121).is_err());
    let late = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithSession, &Answer::good().response(&challenge_of(&late)), 100 + 301).is_err());
}

#[test]
fn a_refused_answer_uses_up_its_challenge_and_a_login_challenge_is_not_accepted() {
    let f = fixture("spent");
    let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    let challenge = challenge_of(&options);
    let mut bad = Answer::good();
    bad.origin = "https://evil.example".into();
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &bad.response(&challenge), 110).is_err());
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &Answer::good().response(&challenge), 111).is_err());
    let unknown = encode_b64url(&[9; 32]);
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &Answer::good().response(&unknown), 112).is_err());
}

#[test]
fn garbage_answers_are_refused_without_a_panic() {
    let f = fixture("garbage");
    let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
    let good = Answer::good().response(&challenge_of(&options));
    for (client, attestation) in [("!!", good.attestation_object.as_str()), (good.client_data_json.as_str(), "!!"), (good.client_data_json.as_str(), ""), ("", ""), (good.client_data_json.as_str(), "oA")] {
        let response = RegistrationResponse { client_data_json: client.into(), attestation_object: attestation.into(), label: "x".into() };
        assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &response, 110).is_err());
    }
    let huge = RegistrationResponse { client_data_json: "A".repeat(10_000), attestation_object: good.attestation_object.clone(), label: "x".into() };
    assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &huge, 110).is_err());
    // Deeply nested and indefinite-length CBOR.
    for raw in [vec![0x9f, 0xff], [vec![0x81; 50], vec![0]].concat()] {
        let response = RegistrationResponse { client_data_json: good.client_data_json.clone(), attestation_object: encode_b64url(&raw), label: "x".into() };
        assert!(finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithToken, &response, 110).is_err());
    }
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn a_token_enrolment_cannot_add_a_second_key_but_a_session_one_can() {
    let f = fixture("second");
    assert!(try_register(&f, &Answer::good(), 120).is_ok());
    let mut second = Answer::good();
    second.credential_id = vec![8; 32];
    assert!(try_register(&f, &second, 121).is_err(), "the token path only fills an empty store");
    let options = begin(&f.host, &f.challenges, &f.store, 130).unwrap();
    let done = finish(&f.host, &f.challenges, &f.authorizer, &f.store, Enrolment::WithSession, &RegistrationResponse { label: "phone".into(), ..second.response(&challenge_of(&options)) }, 131);
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(f.store.list().unwrap().len(), 2);
}
