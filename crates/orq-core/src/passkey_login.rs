//! The server's side of signing in with a passkey: it hands out a single-use challenge and, when the
//! browser answers with an assertion, checks everything before it issues a session. Every refusal is
//! the same value and sets no session, so the answer reveals nothing about which check failed.

use crate::p256;
use crate::passkey_challenge::{decode_b64url, Challenges, Purpose};
use crate::passkey_register::{encoded_len, es256_coordinates, PasskeyHost};
use crate::passkey_store::PasskeyStore;
use crate::session::{Issued, PasskeySignIn, Sessions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_CLIENT_DATA: usize = 4 * 1024;
const MAX_AUTHENTICATOR_DATA: usize = 1024;
const MAX_SIGNATURE: usize = 128;
const MAX_CREDENTIAL_ID_TEXT: usize = 1024;

/// What the browser sends back, as the base64url text it was given as.
pub struct AssertionResponse {
    pub credential_id: String,
    pub client_data_json: String,
    pub authenticator_data: String,
    pub signature: String,
}

/// Why nothing was signed in is deliberately not said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

/// The options for `navigator.credentials.get`, with a fresh challenge. With no passkey registered
/// there is nothing to sign in with, and no challenge is issued.
pub fn begin(host: &PasskeyHost, challenges: &Challenges, store: &PasskeyStore, now: u64) -> Result<Value, String> {
    let keys = store.list()?;
    if keys.is_empty() {
        return Err("no passkey is registered".into());
    }
    let allowed: Vec<Value> = keys.iter().map(|key| json!({"type": "public-key", "id": key.credential_id})).collect();
    Ok(json!({
        "challenge": challenges.issue(Purpose::Login, now)?,
        "rpId": host.rp_id(),
        "userVerification": "required",
        "timeout": 60_000,
        "allowCredentials": allowed,
    }))
}

/// Checks the assertion and, only if every check holds, issues a session. The challenge is spent
/// whatever the outcome, and the signature counter moves forward before the session is issued.
pub fn finish(host: &PasskeyHost, challenges: &Challenges, store: &PasskeyStore, sessions: &Sessions, response: &AssertionResponse, now: u64) -> Result<Issued, Refused> {
    if response.client_data_json.len() > encoded_len(MAX_CLIENT_DATA) {
        return Err(Refused);
    }
    let client_data = decode_b64url(&response.client_data_json).map_err(|_| Refused)?;
    if client_data.len() > MAX_CLIENT_DATA {
        return Err(Refused);
    }
    let data: Value = serde_json::from_slice(&client_data).map_err(|_| Refused)?;
    let text = |name: &str| data.get(name).and_then(Value::as_str);
    // Spent first, so a refused answer cannot be tried again.
    if !challenges.spend(Purpose::Login, text("challenge").ok_or(Refused)?, now) {
        return Err(Refused);
    }
    if text("type") != Some("webauthn.get") || text("origin") != Some(host.origin()) || data.get("crossOrigin").is_some_and(|v| v != &Value::Bool(false)) {
        return Err(Refused);
    }
    if response.credential_id.len() > MAX_CREDENTIAL_ID_TEXT || response.authenticator_data.len() > encoded_len(MAX_AUTHENTICATOR_DATA) || response.signature.len() > encoded_len(MAX_SIGNATURE) {
        return Err(Refused);
    }
    let authenticator_data = decode_b64url(&response.authenticator_data).map_err(|_| Refused)?;
    let signature = decode_b64url(&response.signature).map_err(|_| Refused)?;
    if authenticator_data.len() < 37 || authenticator_data.len() > MAX_AUTHENTICATOR_DATA || signature.len() > MAX_SIGNATURE {
        return Err(Refused);
    }
    let key = store.find(&response.credential_id).map_err(|_| Refused)?.ok_or(Refused)?;
    if authenticator_data[..32] != Sha256::digest(host.rp_id().as_bytes())[..] {
        return Err(Refused);
    }
    let flags = authenticator_data[32];
    // Backed up (0x10) without being eligible for backup (0x08) is not a state an authenticator has.
    if flags & 0x01 == 0 || flags & 0x04 == 0 || (flags & 0x10 != 0 && flags & 0x08 == 0) {
        return Err(Refused);
    }
    let counter = u32::from_be_bytes([authenticator_data[33], authenticator_data[34], authenticator_data[35], authenticator_data[36]]);
    // A counter that does not go forward means a cloned authenticator; a pair of zeros means the
    // authenticator keeps no counter.
    if (key.sign_count != 0 || counter != 0) && counter <= key.sign_count {
        return Err(Refused);
    }
    let cose = decode_b64url(&key.public_key).map_err(|_| Refused)?;
    let (x, y) = es256_coordinates(&cose).map_err(|_| Refused)?;
    let digest: [u8; 32] = Sha256::new().chain_update(&authenticator_data).chain_update(Sha256::digest(&client_data)).finalize().into();
    if !p256::verify(&x, &y, &digest, &signature) {
        return Err(Refused);
    }
    // Always under the lock, even when the counter stays at zero: it also confirms the passkey was
    // not removed while the signature was being checked.
    store.advance_counter(&key, counter).map_err(|_| Refused)?;
    sessions.issue(PasskeySignIn::verified(), now).map_err(|_| Refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p256::tests::{hex, sign, KEY_X, KEY_Y};
    use crate::passkey_challenge::encode_b64url;
    use crate::passkey_store::Passkey;
    use crate::storage::StorageRoots;

    const HOST: &str = "box.tailnet.ts.net";
    const ID: &[u8] = &[5; 32];

    fn head(major: u8, n: usize) -> Vec<u8> {
        let m = major << 5;
        if n < 24 { vec![m | n as u8] } else { vec![m | 24, n as u8] }
    }

    fn cose() -> Vec<u8> {
        let bytes = |b: Vec<u8>| [head(2, b.len()), b].concat();
        [vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21], bytes(hex(KEY_X)), vec![0x22], bytes(hex(KEY_Y))].concat()
    }

    struct Fixture {
        host: PasskeyHost,
        challenges: Challenges,
        store: PasskeyStore,
        sessions: Sessions,
    }

    fn fixture(tag: &str, counter: u32) -> Fixture {
        let base = std::env::temp_dir().join(format!("orq-login-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        let store = PasskeyStore::open(&roots);
        store.add(Passkey { credential_id: encode_b64url(ID), public_key: encode_b64url(&cose()), sign_count: counter, label: "laptop".into(), created_at: 1 }).unwrap();
        Fixture { host: PasskeyHost::from_setting(HOST).unwrap(), challenges: Challenges::new(), store, sessions: Sessions::new(60) }
    }

    /// What an authenticator would send for `challenge`, each part open to change by a test.
    struct Assertion {
        rp_id: String,
        flags: u8,
        counter: u32,
        client_type: String,
        origin: String,
        cross_origin: Option<bool>,
        credential: Vec<u8>,
    }

    impl Assertion {
        fn good(counter: u32) -> Assertion {
            Assertion { rp_id: HOST.into(), flags: 0x05, counter, client_type: "webauthn.get".into(), origin: format!("https://{HOST}"), cross_origin: Some(false), credential: ID.to_vec() }
        }

        fn response(&self, challenge: &str) -> AssertionResponse {
            let mut auth = Sha256::digest(self.rp_id.as_bytes()).to_vec();
            auth.push(self.flags);
            auth.extend(self.counter.to_be_bytes());
            let mut client = json!({"type": self.client_type, "challenge": challenge, "origin": self.origin});
            if let Some(cross) = self.cross_origin {
                client["crossOrigin"] = cross.into();
            }
            let client = client.to_string();
            let digest: [u8; 32] = Sha256::new().chain_update(&auth).chain_update(Sha256::digest(client.as_bytes())).finalize().into();
            AssertionResponse { credential_id: encode_b64url(&self.credential), client_data_json: encode_b64url(client.as_bytes()), authenticator_data: encode_b64url(&auth), signature: encode_b64url(&sign(&digest)) }
        }
    }

    fn challenge(f: &Fixture, now: u64) -> String {
        begin(&f.host, &f.challenges, &f.store, now).unwrap()["challenge"].as_str().unwrap().to_string()
    }

    fn sign_in(f: &Fixture, assertion: &Assertion, now: u64) -> Result<Issued, Refused> {
        let challenge = challenge(f, 100);
        finish(&f.host, &f.challenges, &f.store, &f.sessions, &assertion.response(&challenge), now)
    }

    #[test]
    fn the_options_name_the_host_list_the_credentials_and_need_a_registered_passkey() {
        let f = fixture("options", 0);
        let options = begin(&f.host, &f.challenges, &f.store, 100).unwrap();
        assert_eq!(options["rpId"], HOST);
        assert_eq!(options["userVerification"], "required");
        assert_eq!(options["allowCredentials"][0]["id"], encode_b64url(ID));
        assert_ne!(options["challenge"], begin(&f.host, &f.challenges, &f.store, 100).unwrap()["challenge"]);
        f.store.remove("laptop").unwrap();
        assert!(begin(&f.host, &f.challenges, &f.store, 100).is_err());
    }

    #[test]
    fn a_good_assertion_issues_a_live_session_and_moves_the_counter() {
        let f = fixture("good", 4);
        let issued = sign_in(&f, &Assertion::good(5), 120).unwrap();
        assert!(f.sessions.is_live(&issued.id, 121));
        assert_eq!(f.store.list().unwrap()[0].sign_count, 5);
    }

    #[test]
    fn an_authenticator_without_a_counter_can_sign_in_again_and_again() {
        let f = fixture("zero", 0);
        assert!(sign_in(&f, &Assertion::good(0), 120).is_ok());
        assert!(sign_in(&f, &Assertion::good(0), 121).is_ok());
        assert_eq!(f.store.list().unwrap()[0].sign_count, 0);
    }

    #[test]
    fn every_failed_check_is_the_same_refusal_with_no_session_and_no_counter_change() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Assertion)>)> = vec![
            ("wrong type", Box::new(|a| a.client_type = "webauthn.create".into())),
            ("wrong origin", Box::new(|a| a.origin = "https://evil.example".into())),
            ("origin with port", Box::new(|a| a.origin = format!("https://{HOST}:8443"))),
            ("cross origin", Box::new(|a| a.cross_origin = Some(true))),
            ("wrong rp id", Box::new(|a| a.rp_id = "other.tailnet.ts.net".into())),
            ("no presence", Box::new(|a| a.flags = 0x04)),
            ("no verification", Box::new(|a| a.flags = 0x01)),
            ("backed up but not eligible", Box::new(|a| a.flags = 0x15)),
            ("unknown credential", Box::new(|a| a.credential = vec![6; 32])),
            ("counter equal", Box::new(|a| a.counter = 4)),
            ("counter backwards", Box::new(|a| a.counter = 3)),
        ];
        for (name, change) in cases {
            let f = fixture("refuse", 4);
            let mut assertion = Assertion::good(5);
            change(&mut assertion);
            assert_eq!(sign_in(&f, &assertion, 120), Err(Refused), "{name}");
            assert_eq!(f.sessions.count(), 0, "{name} issued a session");
            assert_eq!(f.store.list().unwrap()[0].sign_count, 4, "{name} moved the counter");
        }
    }

    #[test]
    fn a_bad_signature_is_refused_whether_the_signature_or_the_signed_data_changed() {
        let f = fixture("signature", 0);
        let challenge = challenge(&f, 100);
        let good = Assertion::good(1).response(&challenge);
        let mut flipped = decode_b64url(&good.signature).unwrap();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        let tampered_signature = AssertionResponse { signature: encode_b64url(&flipped), ..good };
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &tampered_signature, 120), Err(Refused));
        // The counter in the authenticator data is covered by the signature.
        let challenge = self::challenge(&f, 100);
        let signed_for_one = Assertion::good(1).response(&challenge);
        let mut auth = decode_b64url(&signed_for_one.authenticator_data).unwrap();
        auth[36] = 2;
        let tampered_data = AssertionResponse { authenticator_data: encode_b64url(&auth), ..signed_for_one };
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &tampered_data, 120), Err(Refused));
        assert_eq!(f.sessions.count(), 0);
    }

    #[test]
    fn a_challenge_works_once_expires_and_is_spent_by_a_refusal() {
        let f = fixture("challenge", 0);
        let first = challenge(&f, 100);
        let response = Assertion::good(1).response(&first);
        assert!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &response, 120).is_ok());
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &response, 121), Err(Refused), "replay");
        let old = challenge(&f, 100);
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &Assertion::good(2).response(&old), 100 + 301), Err(Refused), "expired");
        let spent = challenge(&f, 100);
        let mut wrong = Assertion::good(2);
        wrong.origin = "https://evil.example".into();
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &wrong.response(&spent), 130), Err(Refused));
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &Assertion::good(2).response(&spent), 131), Err(Refused), "the refusal spent it");
        // A registration challenge is not a sign-in challenge.
        let register = crate::passkey_register::begin(&f.host, &f.challenges, &f.store, 100).unwrap()["challenge"].as_str().unwrap().to_string();
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &Assertion::good(2).response(&register), 132), Err(Refused));
    }

    #[test]
    fn backup_flags_are_accepted_when_consistent() {
        let f = fixture("backup", 0);
        for (flags, counter) in [(0x0d, 1), (0x1d, 2), (0x05, 3)] {
            let mut assertion = Assertion::good(counter);
            assertion.flags = flags;
            assert!(sign_in(&f, &assertion, 120).is_ok(), "{flags:#x}");
        }
    }

    #[test]
    fn a_counterless_sign_in_checks_the_store_without_rewriting_it() {
        let f = fixture("no-write", 0);
        let path = PasskeyStore::path(&StorageRoots { data: "d".into(), config: "c".into(), state: std::env::temp_dir().join(format!("orq-login-{}-no-write", std::process::id())).join("state"), cache: "x".into() });
        let before = std::fs::metadata(&path).unwrap();
        let inode = std::os::unix::fs::MetadataExt::ino(&before);
        assert!(sign_in(&f, &Assertion::good(0), 120).is_ok());
        assert_eq!(std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap()), inode, "the file was replaced");
        // A moving counter does replace it.
        let mut moving = Assertion::good(3);
        moving.flags = 0x05;
        assert!(sign_in(&f, &moving, 121).is_ok());
        assert_ne!(std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap()), inode);
    }

    #[test]
    fn a_second_sign_in_with_a_counter_already_used_is_refused() {
        let f = fixture("reuse", 4);
        assert!(sign_in(&f, &Assertion::good(5), 120).is_ok());
        assert_eq!(sign_in(&f, &Assertion::good(5), 121), Err(Refused));
        assert_eq!(f.sessions.count(), 1);
    }

    #[test]
    fn of_two_simultaneous_sign_ins_with_the_same_counter_exactly_one_succeeds() {
        for round in 0..5 {
            let f = fixture(&format!("race-{round}"), 4);
            let responses = [Assertion::good(5).response(&challenge(&f, 100)), Assertion::good(5).response(&challenge(&f, 100))];
            let barrier = std::sync::Barrier::new(2);
            let results: Vec<_> = std::thread::scope(|scope| {
                let handles: Vec<_> = responses.iter().map(|response| scope.spawn(|| {
                    barrier.wait();
                    finish(&f.host, &f.challenges, &f.store, &f.sessions, response, 120)
                })).collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "round {round}");
            assert_eq!((f.sessions.count(), f.store.list().unwrap()[0].sign_count), (1, 5));
        }
    }

    #[test]
    fn two_counter_advances_from_the_same_snapshot_let_exactly_one_through_and_a_removed_passkey_none() {
        let f = fixture("advance", 4);
        let seen = f.store.find(&encode_b64url(ID)).unwrap().unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (store, seen, barrier) = (&f.store, &seen, &barrier);
        let results: Vec<_> = std::thread::scope(|scope| {
            let one = scope.spawn(move || {
                barrier.wait();
                store.advance_counter(seen, 5)
            });
            let two = scope.spawn(move || {
                barrier.wait();
                store.advance_counter(seen, 6)
            });
            vec![one.join().unwrap(), two.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(matches!(f.store.list().unwrap()[0].sign_count, 5 | 6));
        f.store.remove("laptop").unwrap();
        assert!(f.store.advance_counter(&seen, 7).is_err());
    }

    #[test]
    fn a_counterless_passkey_removed_before_the_session_is_issued_gets_none() {
        let f = fixture("revoked", 0);
        let seen = f.store.find(&encode_b64url(ID)).unwrap().unwrap();
        f.store.remove("laptop").unwrap();
        // What finish does after the signature checks out, with the passkey gone in between.
        assert!(f.store.advance_counter(&seen, 0).is_err());
    }

    #[test]
    fn oversized_and_garbage_answers_are_refused_without_a_panic() {
        let f = fixture("garbage", 0);
        let good = |f: &Fixture| Assertion::good(1).response(&challenge(f, 100));
        let edits: Vec<(&str, Box<dyn Fn(&mut AssertionResponse)>)> = vec![
            ("client data not base64", Box::new(|r| r.client_data_json = "!!".into())),
            ("client data huge", Box::new(|r| r.client_data_json = "A".repeat(10_000))),
            ("client data not json", Box::new(|r| r.client_data_json = encode_b64url(b"nope"))),
            ("auth data short", Box::new(|r| r.authenticator_data = encode_b64url(&[0; 36]))),
            ("auth data huge", Box::new(|r| r.authenticator_data = "A".repeat(5_000))),
            ("auth data not base64", Box::new(|r| r.authenticator_data = "!!".into())),
            ("signature empty", Box::new(|r| r.signature = String::new())),
            ("signature huge", Box::new(|r| r.signature = "A".repeat(5_000))),
            ("credential huge", Box::new(|r| r.credential_id = "A".repeat(2_000))),
            ("credential empty", Box::new(|r| r.credential_id = String::new())),
        ];
        for (name, edit) in edits {
            let mut response = good(&f);
            edit(&mut response);
            assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &response, 120), Err(Refused), "{name}");
        }
        assert_eq!(f.sessions.count(), 0);
    }

    #[test]
    fn an_oversized_signature_still_spends_the_challenge() {
        let f = fixture("spent-big", 0);
        let challenge = challenge(&f, 100);
        let good = Assertion::good(1).response(&challenge);
        let big = AssertionResponse { signature: "A".repeat(5_000), credential_id: good.credential_id.clone(), client_data_json: good.client_data_json.clone(), authenticator_data: good.authenticator_data.clone() };
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &big, 120), Err(Refused));
        assert_eq!(finish(&f.host, &f.challenges, &f.store, &f.sessions, &good, 121), Err(Refused));
    }
}
