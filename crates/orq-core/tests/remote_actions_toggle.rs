//! POST /api/settings/remote-actions turns remote actions off from any signed-in request, outside the
//! remote_actions gate, only while the settings version it presents is current; every action route then
//! answers 404, and each attempt is audited as settings.remote_actions.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, public_settings, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome, MAX_ACTION_BODY};
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
