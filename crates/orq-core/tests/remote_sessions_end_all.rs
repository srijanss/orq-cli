//! POST /auth/sessions/end-all ends every passkey session, this one included, for a passkey session presenting
//! its CSRF token from this server's origin. The answer clears this session's cookie. A bearer token is not
//! a session and is refused. Every attempt is audited as sessions.end_all.

mod passkey_support;

use passkey_support::{Device, Rig, TOKEN};
use serde_json::Value;

const PATH: &str = "/auth/sessions/end-all";

fn audit(rig: &Rig) -> Vec<Value> {
    std::fs::read_to_string(rig.roots.state.join("remote-actions.jsonl")).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// A rig with one passkey signed in twice: (rig, this cookie, other cookie, origin, csrf).
fn two_sessions(tag: &str) -> (Rig, String, String, String, String) {
    let rig = Rig::new(tag);
    let device = Device::new(3);
    rig.enrol(&device);
    let cookie = rig.sign_in(&device, 1);
    let other = rig.sign_in(&device, 2);
    let csrf = rig.csrf(&cookie);
    let origin = rig.auth.host().origin().to_string();
    (rig, cookie, other, origin, csrf)
}

#[test]
fn a_passkey_session_ends_every_session_including_its_own() {
    let (rig, cookie, other, origin, csrf) = two_sessions("end-all-ok");
    let reply = rig.change("POST", PATH, Some(&cookie), Some(&origin), Some(&csrf), b"{}");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body["signed_out"], true);
    assert_eq!(reply.body["ended"], 2);
    assert!(reply.set_cookie.as_deref().is_some_and(|header| header.contains("Max-Age=0")), "{:?}", reply.set_cookie);
    assert!(!rig.auth.has_session(Some(&cookie)));
    assert!(!rig.auth.has_session(Some(&other)));
    let lines = audit(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["action"], "sessions.end_all");
    assert_eq!(lines[0]["outcome"], "allowed");
    assert_eq!(lines[0]["identity"], "device 3");
    assert_eq!(lines[0]["reason"], "ended 2 sessions");
}

#[test]
fn a_bearer_token_or_a_request_without_csrf_ends_nothing() {
    let (rig, cookie, other, origin, csrf) = two_sessions("end-all-refused");
    let bearer = format!("Bearer {TOKEN}");
    let reply = rig.call("POST", PATH, Some(&bearer), None, b"{}");
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(reply.body["error"], "Signing out every device needs a passkey sign-in");
    for (origin, csrf) in [(Some(origin.as_str()), None), (Some(origin.as_str()), Some("wrong")), (Some("https://evil.example"), Some(csrf.as_str()))] {
        assert_eq!(rig.change("POST", PATH, Some(&cookie), origin, csrf, b"{}").status, 403, "{origin:?} {csrf:?}");
    }
    assert!(rig.auth.has_session(Some(&cookie)) && rig.auth.has_session(Some(&other)), "every session goes on");
    let lines = audit(&rig);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines.iter().all(|line| line["action"] == "sessions.end_all" && line["outcome"] == "refused"));
}

#[test]
fn only_post_ends_sessions() {
    let (rig, cookie, _, origin, csrf) = two_sessions("end-all-get");
    let reply = rig.change("GET", PATH, Some(&cookie), Some(&origin), Some(&csrf), b"");
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert!(rig.auth.has_session(Some(&cookie)));
}

#[test]
fn no_session_at_all_is_refused_and_audited_as_web() {
    let (rig, cookie, _, origin, _) = two_sessions("end-all-none");
    let reply = rig.change("POST", PATH, None, Some(&origin), None, b"{}");
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert!(rig.auth.has_session(Some(&cookie)));
    let lines = audit(&rig);
    assert_eq!(lines[0]["identity"], "web");
    assert_eq!(lines[0]["reason"], "needs a passkey sign-in");
}
