//! The server's own controls on the web: GET /api/settings carries the server's status (its build, the
//! installed one, any restart it waits for); POST /api/server/restart asks it to restart once no run is
//! active, which needs remote actions on and a passkey session; POST /api/server/restart/cancel drops a
//! pending restart from any signed-in request. Each attempt is audited.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, handle_with_confirm, public_settings, ProjectsProvider, RemoteConfig, Request, ServerControl};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

#[derive(Default)]
struct Control {
    pending: AtomicBool,
    asked: AtomicUsize,
    refuse: Option<String>,
}

impl ServerControl for Control {
    fn status(&self) -> Value {
        json!({"running": "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)", "installed": "orq 1.0.1 (bbbbbbb 2026-02-01T00:00:00Z)", "newer_installed": true, "restart_pending": self.pending.load(Ordering::SeqCst), "waiting_for": 2})
    }

    fn restart_when_idle(&self) -> Result<usize, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(why) = &self.refuse {
            return Err(why.clone());
        }
        self.pending.store(true, Ordering::SeqCst);
        Ok(2)
    }

    fn cancel_restart(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }

    fn restart_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
}

struct Fake {
    config: std::path::PathBuf,
    control: Option<Control>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn settings(&self) -> Result<Option<Value>, String> {
        public_settings(&self.config).map(Some)
    }

    fn server(&self) -> Option<&dyn ServerControl> {
        self.control.as_ref().map(|control| control as &dyn ServerControl)
    }
}

fn setup(tag: &str, actions: bool, control: Option<Control>) -> (StorageRoots, Fake) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-server-restart-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), format!("remote_actions = {actions}\n")).unwrap();
    let fake = Fake { config: roots.config_file(), control };
    (roots, fake)
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let reply = handle_with_body(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, body, fake);
    (reply.status, reply.body)
}

#[test]
fn the_settings_carry_the_servers_status() {
    let (roots, fake) = setup("status", false, Some(Control::default()));
    let (status, body) = send(&roots, &fake, "GET", "/api/settings", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["server"]["running"], "orq 1.0.0 (aaaaaaa 2026-01-01T00:00:00Z)", "{body}");
    assert_eq!(body["server"]["newer_installed"], true, "{body}");
    assert_eq!(body["settings"]["remote_actions"], false, "the settings are still there: {body}");
}

/// The same request from a passkey session, which only such a session's request is given.
fn send_as_session(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let confirm = |_: &Value| true;
    let reply = handle_with_confirm(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, body, fake, Some(&confirm));
    (reply.status, reply.body)
}

fn restart_lines(roots: &StorageRoots, action: &str) -> Vec<Value> {
    remote_audit::read(roots, 100).unwrap().into_iter().filter(|line| line["action"] == action).collect()
}

const RESTART: &str = "/api/server/restart";
const CANCEL: &str = "/api/server/restart/cancel";

#[test]
fn a_passkey_session_with_actions_on_asks_for_a_restart_when_idle() {
    let (roots, fake) = setup("asks", true, Some(Control::default()));
    let (status, body) = send_as_session(&roots, &fake, "POST", RESTART, b"{}");
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["restart_pending"], true, "{body}");
    assert_eq!(body["waiting_for"], 2, "{body}");
    assert!(fake.control.as_ref().unwrap().restart_pending());
    let lines = restart_lines(&roots, "server.restart");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "allowed", "{lines:?}");
}

#[test]
fn a_restart_needs_a_passkey_session_and_remote_actions_and_takes_no_fields() {
    let (roots, fake) = setup("gates", true, Some(Control::default()));
    let (status, body) = send(&roots, &fake, "POST", RESTART, b"{}");
    assert_eq!(status, 403, "the token alone may not restart: {body}");
    assert!(body["error"].as_str().unwrap().contains("passkey"), "{body}");
    let (status, body) = send_as_session(&roots, &fake, "POST", RESTART, br#"{"now": true}"#);
    assert_eq!(status, 400, "{body}");
    let (roots_off, fake_off) = setup("gates-off", false, Some(Control::default()));
    let (status, body) = send_as_session(&roots_off, &fake_off, "POST", RESTART, b"");
    assert_eq!(status, 403, "{body}");
    assert!(body["error"].as_str().unwrap().contains("remote actions"), "{body}");
    for (roots, fake) in [(&roots, &fake), (&roots_off, &fake_off)] {
        assert_eq!(fake.control.as_ref().unwrap().asked.load(Ordering::SeqCst), 0, "the server is never asked");
        let lines = restart_lines(roots, "server.restart");
        assert!(!lines.is_empty() && lines.iter().all(|line| line["outcome"] == "refused"), "{lines:?}");
    }
}

#[test]
fn a_restart_the_server_refuses_says_why() {
    let control = Control { refuse: Some("orq runs from a build under target/".into()), ..Control::default() };
    let (roots, fake) = setup("refused", true, Some(control));
    let (status, body) = send_as_session(&roots, &fake, "POST", RESTART, b"");
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "orq runs from a build under target/", "{body}");
    assert_eq!(restart_lines(&roots, "server.restart")[0]["outcome"], "refused");
}

#[test]
fn cancel_drops_a_pending_restart_from_any_signed_in_request_even_with_actions_off() {
    let (roots, fake) = setup("cancel", false, Some(Control::default()));
    fake.control.as_ref().unwrap().pending.store(true, Ordering::SeqCst);
    let (status, body) = send(&roots, &fake, "POST", CANCEL, b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["cancelled"], true, "{body}");
    assert!(!fake.control.as_ref().unwrap().restart_pending());
    let (status, body) = send(&roots, &fake, "POST", CANCEL, b"");
    assert_eq!((status, body["cancelled"].clone()), (200, json!(false)), "nothing was pending: {body}");
    let lines = restart_lines(&roots, "server.restart_cancel");
    assert_eq!(lines.len(), 2, "{lines:?}");
}

struct WithFeatures(Fake);

impl ProjectsProvider for WithFeatures {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(Some(json!([])))
    }

    fn server(&self) -> Option<&dyn ServerControl> {
        self.0.server()
    }
}

#[test]
fn the_features_say_whether_a_restart_is_pending_so_a_start_can_ask_first() {
    let (roots, fake) = setup("features", true, Some(Control::default()));
    let provider = WithFeatures(fake);
    let read = |provider: &WithFeatures| {
        let request = Request { method: "GET", path: "/api/projects/p/features", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        handle_with_body(&roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, b"", provider).body
    };
    assert_eq!(read(&provider)["restart_pending"], false);
    provider.0.control.as_ref().unwrap().pending.store(true, Ordering::SeqCst);
    assert_eq!(read(&provider)["restart_pending"], true);
}
