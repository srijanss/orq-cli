//! Passkeys are managed from a passkey session only: POST /auth/passkeys/{id}/label {label} renames one
//! by credential id. The label is 1 to 40 characters of printable text and no other passkey's. A bearer
//! token, a request without the session's CSRF token, or one from another origin is refused, and every
//! attempt is audited as passkey.rename.

mod passkey_support;

use orq_core::passkey_store::PasskeyStore;
use passkey_support::{Device, Rig, TOKEN};
use serde_json::{json, Value};

fn audit(rig: &Rig) -> Vec<Value> {
    std::fs::read_to_string(rig.roots.state.join("remote-actions.jsonl")).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn labels(rig: &Rig) -> Vec<String> {
    PasskeyStore::open(&rig.roots).list().unwrap().into_iter().map(|key| key.label).collect()
}

fn id_of(rig: &Rig) -> String {
    PasskeyStore::open(&rig.roots).list().unwrap()[0].credential_id.clone()
}

#[test]
fn a_bearer_token_cannot_rename_a_passkey_and_the_attempt_is_audited() {
    let rig = Rig::new("rename-bearer");
    rig.enrol(&Device::new(3));
    let before = labels(&rig);
    let path = format!("/auth/passkeys/{}/label", id_of(&rig));
    let bearer = format!("Bearer {TOKEN}");
    let reply = rig.call("POST", &path, Some(&bearer), None, json!({ "label": "Laptop" }).to_string().as_bytes());
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(reply.body["error"], "Managing passkeys needs a passkey sign-in");
    assert_eq!(labels(&rig), before, "nothing is renamed");
    let lines = audit(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["action"], "passkey.rename");
    assert_eq!(lines[0]["outcome"], "refused");
    assert_eq!(lines[0]["reason"], "needs a passkey sign-in");
}

/// A rig with two enrolled passkeys, signed in with the first: (rig, cookie, origin, csrf).
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

fn id_labelled(rig: &Rig, label: &str) -> String {
    PasskeyStore::open(&rig.roots).list().unwrap().into_iter().find(|key| key.label == label).unwrap().credential_id
}

fn rename(rig: &Rig, cookie: &str, origin: Option<&str>, csrf: Option<&str>, id: &str, body: &str) -> orq_core::passkey_routes::AuthReply {
    rig.change("POST", &format!("/auth/passkeys/{id}/label"), Some(cookie), origin, csrf, body.as_bytes())
}

#[test]
fn a_passkey_session_renames_one_and_the_attempt_is_audited_as_allowed() {
    let (rig, cookie, origin, csrf) = signed_in("rename-ok");
    let id = id_labelled(&rig, "device 4");
    let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, &json!({ "label": "Laptop" }).to_string());
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body["label"], "Laptop");
    assert!(labels(&rig).contains(&"Laptop".to_string()), "{:?}", labels(&rig));
    assert!(!labels(&rig).contains(&"device 4".to_string()));
    let lines = audit(&rig);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "allowed");
    assert_eq!(lines[0]["identity"], "device 3", "the session's own passkey is named");
    assert_eq!(lines[0]["reason"], "renamed device 4 to Laptop");
}

#[test]
fn a_session_without_its_csrf_token_or_from_another_origin_is_refused() {
    let (rig, cookie, origin, csrf) = signed_in("rename-csrf");
    let id = id_labelled(&rig, "device 4");
    let body = json!({ "label": "Laptop" }).to_string();
    for (origin, csrf) in [(Some(origin.as_str()), None), (Some(origin.as_str()), Some("wrong")), (Some("https://evil.example"), Some(csrf.as_str())), (None, Some(csrf.as_str()))] {
        let reply = rename(&rig, &cookie, origin, csrf, &id, &body);
        assert_eq!(reply.status, 403, "{origin:?} {csrf:?}: {}", reply.body);
    }
    assert!(!labels(&rig).contains(&"Laptop".to_string()));
    assert!(audit(&rig).iter().all(|line| line["outcome"] == "refused"));
}

#[test]
fn a_label_that_is_empty_too_long_or_not_printable_is_refused_with_422() {
    let (rig, cookie, origin, csrf) = signed_in("rename-bad");
    let id = id_labelled(&rig, "device 4");
    for label in ["", "   ", &"x".repeat(41), "a\u{7}b", "line\nbreak"] {
        let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, &json!({ "label": label }).to_string());
        assert_eq!(reply.status, 422, "{label:?}: {}", reply.body);
    }
    let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, &json!({ "label": "x".repeat(40) }).to_string());
    assert_eq!(reply.status, 200, "40 characters is fine: {}", reply.body);
}

#[test]
fn a_body_that_is_not_just_a_label_is_refused_with_400() {
    let (rig, cookie, origin, csrf) = signed_in("rename-body");
    let id = id_labelled(&rig, "device 4");
    for body in ["not json", "{}", r#"{"label": 7}"#, r#"{"label": "a", "extra": 1}"#] {
        let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, body);
        assert_eq!(reply.status, 400, "{body}: {}", reply.body);
    }
}

#[test]
fn an_unknown_id_is_404_and_a_label_another_passkey_has_is_409() {
    let (rig, cookie, origin, csrf) = signed_in("rename-conflict");
    let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), "nope", &json!({ "label": "Laptop" }).to_string());
    assert_eq!(reply.status, 404, "{}", reply.body);
    let id = id_labelled(&rig, "device 4");
    let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, &json!({ "label": "device 3" }).to_string());
    assert_eq!(reply.status, 409, "{}", reply.body);
    let reply = rename(&rig, &cookie, Some(&origin), Some(&csrf), &id, &json!({ "label": "device 4" }).to_string());
    assert_eq!(reply.status, 200, "keeping its own name is fine: {}", reply.body);
}

#[test]
fn no_credentials_at_all_is_refused() {
    let rig = Rig::new("rename-none");
    rig.enrol(&Device::new(3));
    let path = format!("/auth/passkeys/{}/label", id_of(&rig));
    let reply = rig.call("POST", &path, None, None, br#"{"label":"Laptop"}"#);
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(audit(&rig)[0]["identity"], "web");
}
