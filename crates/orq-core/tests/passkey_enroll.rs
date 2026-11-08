//! Enrolling a passkey is allowed with the token while none is registered, or from a signed-in
//! session once there is one, and every other request is refused the same way.

use orq_core::passkey_enroll::{EnrolAuthorizer, EnrolRequest, Enrolment, Refused};
use orq_core::passkey_store::{Passkey, PasskeyStore};
use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const TOKEN: &str = "enrol-test-token-0123456789abcdef0123";
const TAILNET: &str = "100.101.102.103";

fn store(tag: &str) -> (PasskeyStore, StorageRoots) {
    let base = std::env::temp_dir().join(format!("orq-enrol-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    (PasskeyStore::open(&roots), roots)
}

fn config() -> RemoteConfig {
    RemoteConfig { token: TOKEN.to_string() }
}

fn key(id: &str, label: &str) -> Passkey {
    Passkey { credential_id: id.into(), public_key: "AQID".into(), sign_count: 0, label: label.into(), created_at: 1 }
}

fn bearer(token: &str) -> Option<String> {
    Some(format!("Bearer {token}"))
}

fn ask(auth: &EnrolAuthorizer, store: &PasskeyStore, peer: &str, authorization: Option<&str>, session_live: bool, now: u64) -> Result<Enrolment, Refused> {
    auth.authorize(store, &config(), &EnrolRequest { peer: peer.parse::<IpAddr>().unwrap(), authorization, session_live }, now)
}

#[test]
fn with_no_passkey_the_current_token_enrols_and_nothing_else_does() {
    let (store, _) = store("first");
    let auth = EnrolAuthorizer::new();
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Ok(Enrolment::WithToken));
    assert_eq!(ask(&auth, &store, "127.0.0.1", bearer(TOKEN).as_deref(), false, 1), Ok(Enrolment::WithToken));
    assert_eq!(ask(&auth, &store, TAILNET, None, false, 1), Err(Refused));
    assert_eq!(ask(&auth, &store, TAILNET, bearer("wrong-token-0123456789abcdef012345678").as_deref(), false, 1), Err(Refused));
    assert_eq!(ask(&auth, &store, TAILNET, Some(TOKEN), false, 1), Err(Refused), "no Bearer prefix");
    // A session cannot exist before a passkey does, so one that claims to is not enough.
    assert_eq!(ask(&auth, &store, TAILNET, None, true, 1), Err(Refused));
}

#[test]
fn a_peer_outside_the_allowed_ranges_is_refused_even_with_the_token() {
    let (store, _) = store("peer");
    let auth = EnrolAuthorizer::new();
    for peer in ["8.8.8.8", "192.168.1.5", "2001:db8::1"] {
        assert_eq!(ask(&auth, &store, peer, bearer(TOKEN).as_deref(), false, 1), Err(Refused), "{peer}");
    }
    store.add(key("a", "one")).unwrap();
    assert_eq!(ask(&auth, &store, "8.8.8.8", None, true, 1), Err(Refused), "and with a session");
}

#[test]
fn the_token_stops_working_when_the_first_passkey_is_stored_and_a_session_takes_over() {
    let (store, _) = store("closes");
    let auth = EnrolAuthorizer::new();
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Ok(Enrolment::WithToken));
    store.add(key("a", "one")).unwrap();
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 2), Err(Refused), "the token no longer enrols");
    assert_eq!(ask(&auth, &store, "127.0.0.1", bearer(TOKEN).as_deref(), false, 2), Err(Refused), "not even on loopback");
    assert_eq!(ask(&auth, &store, TAILNET, None, false, 2), Err(Refused), "no session, no enrolment");
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), true, 2), Ok(Enrolment::WithSession), "a session wins and is not a token enrolment");
    assert_eq!(ask(&auth, &store, TAILNET, None, true, 2), Ok(Enrolment::WithSession));
}

#[test]
fn removing_every_passkey_reopens_token_enrolment() {
    let (store, _) = store("reopens");
    let auth = EnrolAuthorizer::new();
    store.add(key("a", "one")).unwrap();
    store.add(key("b", "two")).unwrap();
    store.remove("one").unwrap();
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Err(Refused), "one is still registered");
    store.remove("two").unwrap();
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Ok(Enrolment::WithToken));
    assert_eq!(ask(&auth, &store, TAILNET, None, true, 1), Err(Refused), "a session with no passkeys behind it is not enough");
}

#[test]
fn a_store_that_cannot_be_trusted_refuses_everyone_including_the_token() {
    let (store, roots) = store("doubt");
    let auth = EnrolAuthorizer::new();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&roots.state).unwrap();
    let path = PasskeyStore::path(&roots);
    for (what, text, mode) in [("garbage", "not json", 0o600), ("readable by others", r#"{"version":1,"passkeys":[]}"#, 0o644)] {
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Err(Refused), "{what}");
        assert_eq!(ask(&auth, &store, TAILNET, None, true, 1), Err(Refused), "{what}, with a session");
    }
}

#[test]
fn wrong_tokens_lock_the_address_out_for_a_while_then_it_may_try_again() {
    let (store, _) = store("limit");
    let auth = EnrolAuthorizer::new();
    let wrong = bearer("wrong-token-0123456789abcdef012345678");
    for _ in 0..5 {
        assert_eq!(ask(&auth, &store, TAILNET, wrong.as_deref(), false, 100), Err(Refused));
    }
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 101), Err(Refused), "even the right token is refused while locked out");
    assert_eq!(ask(&auth, &store, "100.101.102.104", bearer(TOKEN).as_deref(), false, 101), Ok(Enrolment::WithToken), "another address is unaffected");
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 100 + 299), Err(Refused), "still locked just inside the window");
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 100 + 300), Ok(Enrolment::WithToken), "free again after it");
}

#[test]
fn requests_without_a_token_and_a_right_token_do_not_count_against_the_address() {
    let (store, _) = store("nocount");
    let auth = EnrolAuthorizer::new();
    for _ in 0..20 {
        assert_eq!(ask(&auth, &store, TAILNET, None, false, 1), Err(Refused));
    }
    let wrong = bearer("wrong-token-0123456789abcdef012345678");
    for _ in 0..4 {
        let _ = ask(&auth, &store, TAILNET, wrong.as_deref(), false, 1);
    }
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1), Ok(Enrolment::WithToken), "four wrong then the right one");
    for _ in 0..4 {
        let _ = ask(&auth, &store, TAILNET, wrong.as_deref(), false, 2);
    }
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 2), Ok(Enrolment::WithToken), "a right token cleared the count");
}

#[test]
fn many_addresses_cannot_grow_the_memory_without_limit_or_lock_out_a_recent_one_early() {
    let (store, _) = store("many");
    let auth = EnrolAuthorizer::new();
    let wrong = bearer("wrong-token-0123456789abcdef012345678");
    for i in 0..3000u32 {
        let peer = format!("100.{}.{}.{}", 64 + (i >> 16) % 64, (i >> 8) & 255, i & 255);
        let _ = ask(&auth, &store, &peer, wrong.as_deref(), false, 1 + u64::from(i / 100));
    }
    assert_eq!(ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 40), Ok(Enrolment::WithToken));
}

#[test]
fn every_refusal_is_the_same_value() {
    let (store, _) = store("same");
    let auth = EnrolAuthorizer::new();
    store.add(key("a", "one")).unwrap();
    let results = [
        ask(&auth, &store, "8.8.8.8", bearer(TOKEN).as_deref(), true, 1),
        ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1),
        ask(&auth, &store, TAILNET, None, false, 1),
    ];
    assert!(results.iter().all(|r| *r == Err(Refused)));
}

#[test]
fn of_several_token_enrolments_that_race_exactly_one_is_stored() {
    let (store, roots) = store("race");
    let auth = EnrolAuthorizer::new();
    // All eight are authorized while the store is empty, and only then do they complete together.
    let allowed: Vec<Enrolment> = (0..8).map(|_| ask(&auth, &store, TAILNET, bearer(TOKEN).as_deref(), false, 1).expect("allowed while the store is empty")).collect();
    assert!(allowed.iter().all(|how| *how == Enrolment::WithToken));
    let barrier = std::sync::Barrier::new(allowed.len());
    let landed: usize = std::thread::scope(|scope| {
        let (auth, barrier) = (&auth, &barrier);
        let handles: Vec<_> = allowed
            .into_iter()
            .enumerate()
            .map(|(i, how)| {
                let roots = roots.clone();
                scope.spawn(move || {
                    let store = PasskeyStore::open(&roots);
                    barrier.wait();
                    auth.complete(&store, how, key(&format!("cred{i}"), &format!("device{i}"))).is_ok()
                })
            })
            .collect();
        handles.into_iter().map(|h| usize::from(h.join().unwrap())).sum()
    });
    assert_eq!(landed, 1, "exactly one token enrolment lands");
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(auth.complete(&store, Enrolment::WithToken, key("later", "later")).is_err(), "a late token enrolment is refused");
    auth.complete(&store, Enrolment::WithSession, key("second", "second")).expect("a session can add more");
    assert_eq!(store.list().unwrap().len(), 2);
}
