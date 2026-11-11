//! POST /auth/confirm/begin {purpose} hands a signed-in page a challenge tied to that purpose and its
//! passkey session, good for two minutes; an assertion against it (user verification required, from a
//! registered passkey) is accepted once, and refused if reused, expired, or for another purpose or session.

mod passkey_support;

use orq_core::passkey_confirm::{Confirmations, CONFIRM_SECS};
use passkey_support::{Answer, Device, Rig};
use serde_json::{json, Value};

const ON: &str = "remote_actions.on";

fn begin(rig: &Rig, cookie: Option<&str>, purpose: &str) -> (u16, Value) {
    let body = json!({ "purpose": purpose }).to_string();
    let reply = rig.call("POST", "/auth/confirm/begin", None, cookie, body.as_bytes());
    assert!(reply.set_cookie.is_none());
    (reply.status, reply.body)
}

fn challenge(rig: &Rig, cookie: &str) -> String {
    let (status, options) = begin(rig, Some(cookie), ON);
    assert_eq!(status, 200, "{options}");
    options["challenge"].as_str().unwrap().to_string()
}

fn signed_in(tag: &str) -> (Rig, Device, String) {
    let rig = Rig::new(tag);
    let device = Device::new(7);
    rig.enrol(&device);
    let cookie = rig.sign_in(&device, 1);
    (rig, device, cookie)
}

#[test]
fn a_signed_in_page_gets_options_that_need_user_verification_from_a_registered_passkey() {
    let (rig, device, cookie) = signed_in("options");
    let (status, options) = begin(&rig, Some(&cookie), ON);
    assert_eq!(status, 200, "{options}");
    assert_eq!(options["rpId"], passkey_support::RP);
    assert_eq!(options["userVerification"], "required");
    assert_eq!(options["allowCredentials"], json!([{ "type": "public-key", "id": device.assertion("x", &Answer::good(0))["id"] }]));
    assert_eq!(options["timeout"], json!(CONFIRM_SECS * 1000));
    assert_ne!(options["challenge"], begin(&rig, Some(&cookie), ON).1["challenge"], "each begin is a new challenge");
}

#[test]
fn only_a_signed_in_page_naming_a_known_purpose_gets_a_challenge() {
    let (rig, _, cookie) = signed_in("begin-refused");
    assert_eq!(begin(&rig, None, ON).0, 401);
    assert_eq!(begin(&rig, Some("__Host-orq-session=not-a-session"), ON).0, 401);
    assert_eq!(begin(&rig, Some(&cookie), "remote_actions.off").0, 400);
    for body in [&b"{}"[..], b"[]", b"not json", b"", br#"{"purpose": 1}"#, br#"{"purpose": "remote_actions.on", "extra": true}"#] {
        let reply = rig.call("POST", "/auth/confirm/begin", None, Some(&cookie), body);
        assert_eq!(reply.status, 400, "{}", String::from_utf8_lossy(body));
    }
    assert_eq!(rig.call("GET", "/auth/confirm/begin", None, Some(&cookie), b"").status, 404);
}

#[test]
fn a_good_assertion_is_accepted_once_and_moves_the_counter() {
    let (rig, device, cookie) = signed_in("once");
    let assertion = device.assertion(&challenge(&rig, &cookie), &Answer::good(2));
    assert!(rig.auth.confirm(&rig.roots, Some(&cookie), ON, &assertion));
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &assertion), "a challenge is used once");

    // The counter moved with the confirmation, so an answer that does not move it is refused.
    let stale = device.assertion(&challenge(&rig, &cookie), &Answer::good(2));
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &stale));
    let next = device.assertion(&challenge(&rig, &cookie), &Answer::good(3));
    assert!(rig.auth.confirm(&rig.roots, Some(&cookie), ON, &next));
}

#[test]
fn a_challenge_is_for_its_own_purpose_and_session_only_and_a_wrong_try_spends_it() {
    let (rig, device, cookie) = signed_in("bound");
    let other_session = rig.sign_in(&device, 2);

    let issued = challenge(&rig, &cookie);
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), "remote_actions.off", &device.assertion(&issued, &Answer::good(3))));
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(&issued, &Answer::good(4))), "spent by the wrong purpose");

    let issued = challenge(&rig, &cookie);
    assert!(!rig.auth.confirm(&rig.roots, Some(&other_session), ON, &device.assertion(&issued, &Answer::good(5))));
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(&issued, &Answer::good(6))), "spent by the other session");

    let issued = challenge(&rig, &cookie);
    assert!(!rig.auth.confirm(&rig.roots, None, ON, &device.assertion(&issued, &Answer::good(7))), "no session at all");
}

#[test]
fn a_forged_or_unverified_assertion_is_refused() {
    let (rig, device, cookie) = signed_in("forged");
    let stranger = Device::new(9);
    let cases: Vec<(&str, Value)> = vec![
        ("no user verification", device.assertion(&challenge(&rig, &cookie), &Answer { flags: 0x01, ..Answer::good(3) })),
        ("another origin", device.assertion(&challenge(&rig, &cookie), &Answer { origin: "https://evil.example".into(), ..Answer::good(3) })),
        ("a passkey that is not registered", stranger.assertion(&challenge(&rig, &cookie), &Answer::good(3))),
        ("an invented challenge", device.assertion("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", &Answer::good(3))),
        ("not an assertion", json!({ "id": "x" })),
        ("not an object", json!("assertion")),
    ];
    for (name, assertion) in cases {
        assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &assertion), "{name}");
    }
}

#[test]
fn a_refused_answer_spends_its_challenge_so_a_good_answer_to_it_is_refused_too() {
    let (rig, device, cookie) = signed_in("spent");
    let refused = [
        ("bad signature", Answer { flip_signature: true, ..Answer::good(3) }),
        ("no user verification", Answer { flags: 0x01, ..Answer::good(3) }),
        ("another origin", Answer { origin: "https://evil.example".into(), ..Answer::good(3) }),
    ];
    for (name, answer) in refused {
        let issued = challenge(&rig, &cookie);
        assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(&issued, &answer)), "{name}");
        assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(&issued, &Answer::good(3))), "{name}: the challenge is spent");
    }
    // Nothing refused moved the counter, and a fresh challenge still confirms.
    assert!(rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(&challenge(&rig, &cookie), &Answer::good(2))));
}

#[test]
fn sign_in_and_confirmation_challenges_cannot_stand_in_for_each_other() {
    let (rig, device, cookie) = signed_in("crossed");
    let login = rig.call("POST", "/auth/login/begin", None, None, b"{}").body;
    assert!(!rig.auth.confirm(&rig.roots, Some(&cookie), ON, &device.assertion(login["challenge"].as_str().unwrap(), &Answer::good(3))));

    let confirm = challenge(&rig, &cookie);
    let body = device.assertion(&confirm, &Answer::good(4)).to_string();
    let reply = rig.call("POST", "/auth/login/finish", None, None, body.as_bytes());
    assert_eq!(reply.status, 401);
    assert!(reply.set_cookie.is_none());
}

#[test]
fn a_confirmation_challenge_lasts_two_minutes() {
    assert_eq!(CONFIRM_SECS, 120);
    let pending = Confirmations::new();
    let late = pending.issue(ON, "session-a", 1_000).unwrap();
    assert!(!pending.spend(ON, "session-a", &late, 1_000 + CONFIRM_SECS));
    let in_time = pending.issue(ON, "session-a", 1_000).unwrap();
    assert!(pending.spend(ON, "session-a", &in_time, 1_000 + CONFIRM_SECS - 1));
}
