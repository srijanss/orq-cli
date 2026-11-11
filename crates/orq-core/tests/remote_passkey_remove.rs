//! POST /auth/passkeys/{id}/remove removes a passkey by credential id, for a passkey session presenting
//! its CSRF token from this server's origin. The last passkey cannot be removed, every session opened with
//! the removed passkey ends, and every attempt is audited as passkey.remove.

mod passkey_support;

use orq_core::passkey_store::PasskeyStore;
use passkey_support::{Device, Rig, TOKEN};
use serde_json::Value;

fn audit(rig: &Rig) -> Vec<Value> {
    std::fs::read_to_string(rig.roots.state.join("remote-actions.jsonl")).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn labels(rig: &Rig) -> Vec<String> {
    PasskeyStore::open(&rig.roots).list().unwrap().into_iter().map(|key| key.label).collect()
}

fn id_labelled(rig: &Rig, label: &str) -> String {
    PasskeyStore::open(&rig.roots).list().unwrap().into_iter().find(|key| key.label == label).unwrap().credential_id
}

/// A rig with two enrolled passkeys ("device 3" and "device 4"), signed in with the first:
/// (rig, cookie, origin, csrf).
fn signed_in(tag: &str) -> (Rig, String, String, String) {
    let rig = Rig::new(tag);
    let (one, two) = (Device::new(3), Device::new(4));
    rig.enrol(&one);
    let cookie = rig.sign_in(&one, 1);
    let options = rig.call("POST", "/auth/register/begin", None, Some(&cookie), b"{}").body;
    let added = rig.call("POST", "/auth/register/finish", None, Some(&cookie), &two.register_body(options["challenge"].as_str().unwrap()));
    assert_eq!(added.status, 200, "a second passkey is added from the session: {}", added.body);
    let csrf = rig.csrf(&cookie);
    let origin = rig.auth.host().origin().to_string();
    (rig, cookie, origin, csrf)
}

fn remove(rig: &Rig, cookie: &str, origin: Option<&str>, csrf: Option<&str>, id: &str) -> orq_core::passkey_routes::AuthReply {
    rig.change("POST", &format!("/auth/passkeys/{id}/remove"), Some(cookie), origin, csrf, b"{}")
}

#[test]
fn a_passkey_session_removes_another_passkey_and_its_sessions_end() {
    let (rig, cookie, origin, csrf) = signed_in("remove-ok");
    let other = rig.sign_in(&Device::new(4), 1);
    assert!(rig.auth.has_session(Some(&other)), "device 4 is signed in too");
    let id = id_labelled(&rig, "device 4");
    let reply = remove(&rig, &cookie, Some(&origin), Some(&csrf), &id);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body["label"], "device 4");
    assert_eq!(reply.body["signed_out"], false, "this session used another passkey");
    assert_eq!(labels(&rig), vec!["device 3".to_string()]);
    assert!(!rig.auth.has_session(Some(&other)), "the removed passkey's session ended");
    assert!(rig.auth.has_session(Some(&cookie)), "this session goes on");
    let lines = audit(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["action"], "passkey.remove");
    assert_eq!(lines[0]["outcome"], "allowed");
    assert_eq!(lines[0]["identity"], "device 3");
    assert_eq!(lines[0]["reason"], "removed device 4");
}

#[test]
fn the_last_passkey_cannot_be_removed() {
    let rig = Rig::new("remove-last");
    let one = Device::new(3);
    rig.enrol(&one);
    let cookie = rig.sign_in(&one, 1);
    let (origin, csrf) = (rig.auth.host().origin().to_string(), rig.csrf(&cookie));
    let reply = remove(&rig, &cookie, Some(&origin), Some(&csrf), &id_labelled(&rig, "device 3"));
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.body["error"], "Add another passkey before removing this one");
    assert_eq!(labels(&rig), vec!["device 3".to_string()]);
    assert!(rig.auth.has_session(Some(&cookie)));
    assert_eq!(audit(&rig)[0]["outcome"], "refused");
}

#[test]
fn a_bearer_token_or_a_request_without_csrf_cannot_remove_a_passkey() {
    let (rig, cookie, origin, csrf) = signed_in("remove-refused");
    let id = id_labelled(&rig, "device 4");
    let bearer = format!("Bearer {TOKEN}");
    let reply = rig.call("POST", &format!("/auth/passkeys/{id}/remove"), Some(&bearer), None, b"{}");
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(reply.body["error"], "Managing passkeys needs a passkey sign-in");
    for (origin, csrf) in [(Some(origin.as_str()), None), (Some(origin.as_str()), Some("wrong")), (Some("https://evil.example"), Some(csrf.as_str()))] {
        assert_eq!(remove(&rig, &cookie, origin, csrf, &id).status, 403, "{origin:?} {csrf:?}");
    }
    assert_eq!(labels(&rig).len(), 2, "nothing is removed");
    let lines = audit(&rig);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines.iter().all(|line| line["action"] == "passkey.remove" && line["outcome"] == "refused"));
}

#[test]
fn an_unknown_id_is_404() {
    let (rig, cookie, origin, csrf) = signed_in("remove-unknown");
    let reply = remove(&rig, &cookie, Some(&origin), Some(&csrf), "nope");
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert_eq!(labels(&rig).len(), 2);
}

#[test]
fn removing_this_sessions_own_passkey_signs_it_out_and_clears_the_cookie() {
    let (rig, cookie, origin, csrf) = signed_in("remove-own");
    let reply = remove(&rig, &cookie, Some(&origin), Some(&csrf), &id_labelled(&rig, "device 3"));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body["signed_out"], true);
    assert!(reply.set_cookie.as_deref().is_some_and(|header| header.contains("Max-Age=0")), "{:?}", reply.set_cookie);
    assert!(!rig.auth.has_session(Some(&cookie)));
    assert_eq!(labels(&rig), vec!["device 4".to_string()]);
    assert_eq!(audit(&rig)[0]["identity"], "device 3", "the audit names who asked, before the removal");
}

#[test]
fn the_passkey_list_marks_the_one_this_session_signed_in_with() {
    let (rig, cookie, _, _) = signed_in("remove-list");
    let list = rig.call("GET", "/auth/passkeys", None, Some(&cookie), b"").body;
    let marked: Vec<(String, bool)> = list["passkeys"].as_array().unwrap().iter().map(|row| (row["label"].as_str().unwrap().to_string(), row["this_device"].as_bool().unwrap())).collect();
    assert_eq!(marked, vec![("device 3".to_string(), true), ("device 4".to_string(), false)]);
}

#[test]
fn only_post_removes() {
    let (rig, cookie, origin, csrf) = signed_in("remove-get");
    let id = id_labelled(&rig, "device 4");
    let reply = rig.change("GET", &format!("/auth/passkeys/{id}/remove"), Some(&cookie), Some(&origin), Some(&csrf), b"");
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert_eq!(labels(&rig).len(), 2);
}
