//! POST /api/settings/remote-actions turns remote actions off from any signed-in request, outside the
//! remote_actions gate, only while the settings version it presents is current; every action route then
//! answers 404, and each attempt is audited as settings.remote_actions. Turning them on again needs a
//! passkey session and a fresh passkey check (step-up) for that purpose.

mod passkey_support;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, handle_with_confirm, public_settings, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome, MAX_ACTION_BODY};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    config: std::path::PathBuf,
    asked: Mutex<usize>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn settings(&self) -> Result<Option<Value>, String> {
        public_settings(&self.config).map(Some)
    }

    fn control_run(&self, _id: &str, run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        *self.asked.lock().unwrap() += 1;
        Ok(RunControlOutcome::Done(run.into()))
    }
}

fn setup(tag: &str, config: &str) -> (StorageRoots, Fake) {
    let base = std::env::temp_dir().join(format!("orq-actionstoggle-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), config).unwrap();
    let fake = Fake { config: roots.config_file(), asked: Mutex::new(0) };
    (roots, fake)
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> (u16, Value) {
    let authorization = token.map(|token| format!("Bearer {token}"));
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: authorization.as_deref() };
    let reply = handle_with_body(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, body, fake);
    (reply.status, reply.body)
}

fn version(roots: &StorageRoots, fake: &Fake) -> String {
    let (status, body) = send(roots, fake, "GET", "/api/settings", Some("tok"), b"");
    assert_eq!(status, 200, "{body}");
    body["settings"]["version"].as_str().unwrap().to_string()
}

const SWITCH: &str = "/api/settings/remote-actions";
const PAUSE: &str = "/api/projects/p-1/runs/r-1/pause";

fn switch_lines(roots: &StorageRoots) -> Vec<Value> {
    remote_audit::read(roots, 100).unwrap().into_iter().filter(|line| line["action"] == "settings.remote_actions").collect()
}

#[test]
fn turning_actions_off_keeps_the_rest_of_the_file_and_every_action_route_then_answers_404() {
    let (roots, fake) = setup("off", "# mine\nremote_actions = true\n");
    assert_eq!(send(&roots, &fake, "POST", PAUSE, Some("tok"), b"").0, 202);

    let body = json!({"on": false, "version": version(&roots, &fake)}).to_string();
    let (status, reply) = send(&roots, &fake, "POST", SWITCH, Some("tok"), body.as_bytes());
    assert_eq!((status, &reply), (200, &json!({"remote_actions": false})), "{reply}");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "# mine\nremote_actions = false\n");

    assert_eq!(send(&roots, &fake, "POST", PAUSE, Some("tok"), b"").0, 404);
    assert_eq!(*fake.asked.lock().unwrap(), 1, "the pause after the switch never reached the provider");

    // Already off is not behind the gate: turning off again is still answered.
    let body = json!({"on": false, "version": version(&roots, &fake)}).to_string();
    assert_eq!(send(&roots, &fake, "POST", SWITCH, Some("tok"), body.as_bytes()).0, 200);

    let lines = switch_lines(&roots);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "allowed"), "{lines:?}");
}

#[test]
fn a_stale_version_is_refused_with_409_and_nothing_is_written() {
    let (roots, fake) = setup("stale", "remote_actions = true\n");
    let stale = version(&roots, &fake);
    std::fs::write(roots.config_file(), "remote_actions = true\n# edited by hand\n").unwrap();

    let body = json!({"on": false, "version": stale}).to_string();
    let (status, reply) = send(&roots, &fake, "POST", SWITCH, Some("tok"), body.as_bytes());
    assert_eq!((status, reply), (409, json!({"error": "Settings changed since you opened them"})));
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "remote_actions = true\n# edited by hand\n");
    assert_eq!(send(&roots, &fake, "POST", PAUSE, Some("tok"), b"").0, 202, "actions stay on");

    let lines = switch_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("settings changed")));
}

#[test]
fn only_a_signed_in_post_with_the_right_body_reaches_the_switch() {
    let (roots, fake) = setup("bodies", "remote_actions = true\n");
    let current = version(&roots, &fake);
    let good = json!({"on": false, "version": current}).to_string();

    assert_eq!(send(&roots, &fake, "POST", SWITCH, None, good.as_bytes()).0, 401);
    assert_eq!(send(&roots, &fake, "POST", SWITCH, Some("wrong"), good.as_bytes()).0, 401);
    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(send(&roots, &fake, method, SWITCH, Some("tok"), good.as_bytes()).0, 405, "{method}");
    }

    let refused = json!({"error": "the request body is not one this action takes"});
    let bad = [
        json!({"on": false}).to_string(),
        json!({"version": current}).to_string(),
        json!({"on": "false", "version": current}).to_string(),
        json!({"on": false, "version": 1}).to_string(),
        json!({"on": false, "version": current, "remote_control": false}).to_string(),
        "[]".to_string(),
        "not json".to_string(),
        String::new(),
    ];
    for body in &bad {
        assert_eq!(send(&roots, &fake, "POST", SWITCH, Some("tok"), body.as_bytes()), (400, refused.clone()), "{body}");
    }
    let mut large = vec![b' '; MAX_ACTION_BODY];
    large.extend_from_slice(good.as_bytes());
    assert_eq!(send(&roots, &fake, "POST", SWITCH, Some("tok"), &large).0, 413);

    // Turning on needs a passkey check, which a plain request cannot give.
    let on = json!({"on": true, "version": current}).to_string();
    let (status, reply) = send(&roots, &fake, "POST", SWITCH, Some("tok"), on.as_bytes());
    assert_eq!((status, reply), (403, json!({"error": "Turning remote actions on needs a passkey sign-in"})));

    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "remote_actions = true\n", "nothing refused was written");
    let lines = switch_lines(&roots);
    assert_eq!(lines.len(), bad.len() + 2, "every refusal after the token check is audited: {lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"), "{lines:?}");
}

#[test]
fn a_settings_file_that_cannot_be_read_is_left_alone_and_the_attempt_is_refused() {
    let broken = "this is [not toml";
    let (roots, fake) = setup("broken", broken);
    let current = orq_core::remote_settings::version_of(broken);

    let body = json!({"on": false, "version": current}).to_string();
    let (status, reply) = send(&roots, &fake, "POST", SWITCH, Some("tok"), body.as_bytes());
    assert_eq!((status, &reply), (500, &json!({"error": "the settings could not be read"})));
    assert!(!reply.to_string().contains(roots.config.to_str().unwrap()), "{reply}");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), broken);

    let lines = switch_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("the settings could not be read")));
}

/// A signed-in passkey session over `config`, as the transport would see it: the handler gets the
/// token, and a check that answers whether an assertion confirms turning actions on for this session.
struct Signed {
    rig: passkey_support::Rig,
    device: passkey_support::Device,
    cookie: String,
    fake: Fake,
    count: u32,
}

impl Signed {
    fn new(tag: &str, config: &str) -> Signed {
        let rig = passkey_support::Rig::new(&format!("actions-on-{tag}"));
        std::fs::write(rig.roots.config_file(), config).unwrap();
        let device = passkey_support::Device::new(5);
        rig.enrol(&device);
        let cookie = rig.sign_in(&device, 1);
        let fake = Fake { config: rig.roots.config_file(), asked: Mutex::new(0) };
        Signed { rig, device, cookie, fake, count: 1 }
    }

    /// A fresh assertion for turning actions on, as the page gets it from the passkey prompt.
    fn assertion(&mut self) -> Value {
        let cookie = self.cookie.clone();
        self.assertion_for(&cookie)
    }

    /// The same, asked for by the session `cookie`.
    fn assertion_for(&mut self, cookie: &str) -> Value {
        let body = json!({"purpose": "remote_actions.on"}).to_string();
        let options = self.rig.call("POST", "/auth/confirm/begin", None, Some(cookie), body.as_bytes()).body;
        self.count += 1;
        self.device.assertion(options["challenge"].as_str().unwrap(), &passkey_support::Answer::good(self.count))
    }

    fn post(&self, path: &str, body: &Value, session: Option<&str>) -> (u16, Value) {
        let authorization = format!("Bearer {}", passkey_support::TOKEN);
        let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some(&authorization) };
        let confirm = |assertion: &Value| self.rig.auth.confirm(&self.rig.roots, session, "remote_actions.on", assertion);
        let check: Option<&dyn Fn(&Value) -> bool> = session.map(|_| &confirm as &dyn Fn(&Value) -> bool);
        let reply = handle_with_confirm(&self.rig.roots, &self.rig.config, "p", "p", &request, body.to_string().as_bytes(), &self.fake, check);
        (reply.status, reply.body)
    }

    fn version(&self) -> String {
        let authorization = format!("Bearer {}", passkey_support::TOKEN);
        let request = Request { method: "GET", path: "/api/settings", peer: "127.0.0.1".parse().unwrap(), authorization: Some(&authorization) };
        let reply = handle_with_body(&self.rig.roots, &self.rig.config, "p", "p", &request, b"", &self.fake);
        reply.body["settings"]["version"].as_str().unwrap().to_string()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.rig.roots.config_file()).unwrap()
    }
}

#[test]
fn a_passkey_session_with_a_fresh_passkey_check_turns_actions_on_and_they_answer_again() {
    let mut signed = Signed::new("on", "# mine\nremote_actions = false\n");
    let cookie = signed.cookie.clone();
    assert_eq!(signed.post(PAUSE, &json!({}), Some(&cookie)).0, 404);

    let body = json!({"on": true, "version": signed.version(), "assertion": signed.assertion()});
    let (status, reply) = signed.post(SWITCH, &body, Some(&cookie));
    assert_eq!((status, &reply), (200, &json!({"remote_actions": true})), "{reply}");
    assert_eq!(signed.file(), "# mine\nremote_actions = true\n");
    assert_eq!(signed.post(PAUSE, &json!({}), Some(&cookie)).0, 202);

    let lines = switch_lines(&signed.rig.roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "allowed");
}

#[test]
fn turning_on_is_refused_without_a_session_or_without_a_good_unused_passkey_check_for_it() {
    let mut signed = Signed::new("on-refused", "remote_actions = false\n");
    let cookie = signed.cookie.clone();
    let version = signed.version();
    let needs_session = json!({"error": "Turning remote actions on needs a passkey sign-in"});
    let failed = json!({"error": "The passkey check did not pass"});

    // The token alone, even with an assertion that would have passed.
    let body = json!({"on": true, "version": version, "assertion": signed.assertion()});
    assert_eq!(signed.post(SWITCH, &body, None), (403, needs_session.clone()));

    // A session with no assertion, or one that is not an assertion.
    assert_eq!(signed.post(SWITCH, &json!({"on": true, "version": version}), Some(&cookie)), (403, failed.clone()));
    assert_eq!(signed.post(SWITCH, &json!({"on": true, "version": version, "assertion": "yes"}), Some(&cookie)), (403, failed.clone()));

    // Used once already: the second time it is refused.
    let once = signed.assertion();
    let body = json!({"on": true, "version": version, "assertion": once});
    assert_eq!(signed.post(SWITCH, &body, Some(&cookie)).0, 200);
    let version = signed.version();
    let body = json!({"on": true, "version": version, "assertion": once});
    assert_eq!(signed.post(SWITCH, &body, Some(&cookie)), (403, failed.clone()), "reused");

    // Asked for by another session.
    let other = signed.rig.sign_in(&signed.device, 50);
    signed.count = 50;
    let body = json!({"on": true, "version": version, "assertion": signed.assertion_for(&other)});
    assert_eq!(signed.post(SWITCH, &body, Some(&other)).0, 200, "the other session's own check works");
    let body = json!({"on": true, "version": signed.version(), "assertion": signed.assertion_for(&other)});
    assert_eq!(signed.post(SWITCH, &body, Some(&cookie)), (403, failed), "a check asked for by the other session");

    // The audit reads newest first.
    let refused: Vec<_> = switch_lines(&signed.rig.roots).into_iter().rev().filter(|line| line["outcome"] == "refused").map(|line| line["reason"].clone()).collect();
    assert_eq!(refused, vec![json!("needs a passkey sign-in"), json!("passkey check failed"), json!("passkey check failed"), json!("passkey check failed"), json!("passkey check failed")]);
}

#[test]
fn a_good_passkey_check_with_a_stale_version_writes_nothing() {
    let mut signed = Signed::new("on-stale", "remote_actions = false\n");
    let cookie = signed.cookie.clone();
    let stale = signed.version();
    std::fs::write(signed.rig.roots.config_file(), "remote_actions = false\n# edited by hand\n").unwrap();

    let body = json!({"on": true, "version": stale, "assertion": signed.assertion()});
    assert_eq!(signed.post(SWITCH, &body, Some(&cookie)), (409, json!({"error": "Settings changed since you opened them"})));
    assert_eq!(signed.file(), "remote_actions = false\n# edited by hand\n");
}
