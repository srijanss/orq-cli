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

fn rename(rig: &Rig, cookie: &str, origin: &str, csrf: &str, id: &str) -> orq_core::passkey_routes::AuthReply {
    rig.change("POST", &format!("/auth/passkeys/{id}/label"), Some(cookie), Some(origin), Some(csrf), br#"{"label": "Laptop"}"#)
}

/// Puts a directory where `name` is in the state directory, runs `act`, then puts the file back.
fn broken<T>(rig: &Rig, name: &str, act: impl FnOnce() -> T) -> T {
    let path = rig.roots.state.join(name);
    let kept = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir(&path).unwrap();
    let out = act();
    std::fs::remove_dir(&path).unwrap();
    if let Some(bytes) = kept {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap().write_all(&bytes).unwrap();
    }
    out
}

#[test]
fn store_a_passkey_store_that_cannot_be_written_changes_nothing_and_says_only_that_it_failed() {
    let (rig, cookie, origin, csrf) = signed_in("remove-unwritable");
    let other = rig.sign_in(&Device::new(4), 1);
    let id = id_labelled(&rig, "device 4");
    let (removed, renamed) = broken(&rig, ".passkeys.lock", || (remove(&rig, &cookie, Some(&origin), Some(&csrf), &id), rename(&rig, &cookie, &origin, &csrf, &id)));
    for reply in [&removed, &renamed] {
        assert_eq!(reply.status, 500, "{}", reply.body);
        assert_eq!(reply.body["error"], "The passkeys could not be changed");
        assert!(reply.set_cookie.is_none(), "no cookie is cleared: {:?}", reply.set_cookie);
    }
    assert_eq!(labels(&rig), vec!["device 3".to_string(), "device 4".to_string()], "the passkeys are as they were");
    assert!(rig.auth.has_session(Some(&other)) && rig.auth.has_session(Some(&cookie)), "no session ended");
    let lines = audit(&rig);
    let seen: Vec<(&str, &str, &str)> = lines.iter().map(|l| (l["action"].as_str().unwrap(), l["outcome"].as_str().unwrap(), l["reason"].as_str().unwrap())).collect();
    assert_eq!(seen, vec![("passkey.remove", "refused", "the passkeys could not be written"), ("passkey.rename", "refused", "the passkeys could not be written")]);
}

#[test]
fn store_a_passkey_store_that_cannot_be_read_is_not_a_sign_in_and_changes_nothing() {
    let (rig, cookie, origin, csrf) = signed_in("remove-unreadable");
    let id = id_labelled(&rig, "device 4");
    let file = PasskeyStore::path(&rig.roots);
    let name = file.file_name().unwrap().to_str().unwrap().to_string();
    let (removed, renamed) = broken(&rig, &name, || (remove(&rig, &cookie, Some(&origin), Some(&csrf), &id), rename(&rig, &cookie, &origin, &csrf, &id)));
    for reply in [&removed, &renamed] {
        assert_eq!(reply.status, 403, "{}", reply.body);
        assert!(!reply.body.to_string().contains(rig.roots.state.to_str().unwrap()), "no path is shown: {}", reply.body);
        assert!(reply.set_cookie.is_none(), "{:?}", reply.set_cookie);
    }
    assert_eq!(labels(&rig), vec!["device 3".to_string(), "device 4".to_string()]);
    assert!(rig.auth.session_live(&rig.roots, Some(&cookie)), "the session outlives a store it could not read");
    let lines = audit(&rig);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused" && line["identity"] == "web"), "{lines:?}");
}
