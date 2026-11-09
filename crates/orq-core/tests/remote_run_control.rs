//! `POST /api/projects/{id}/runs/{run}/pause` and `.../resume`: act only when remote actions are on, say why
//! when the run will not, and leave one audit line for every attempt that got past the token.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    asked: Mutex<Vec<(String, String, RunControl)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn control_run(&self, id: &str, run: &str, control: RunControl) -> Result<RunControlOutcome, String> {
        match (id, run) {
            ("p-1", "r-ok") => {
                self.asked.lock().unwrap().push((id.into(), run.into(), control));
                Ok(RunControlOutcome::Done(if control == RunControl::Resume { "r-new".into() } else { run.into() }))
            }
            ("p-1", "r-done") => Ok(RunControlOutcome::Refused("r-done is completed; only a running run can be paused".into())),
            ("p-1", "r-broken") => Err("/home/me/secret/path exploded".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-runcontrol-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn post(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, token: Option<&str>) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: token };
    let r = handle_with(roots, &config, "p", "p", &request, fake);
    (r.status, r.body)
}

#[test]
fn a_run_is_paused_or_resumed_only_when_actions_are_on_and_every_attempt_is_audited() {
    let roots = roots("control");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    let url = |run: &str, what: &str| format!("/api/projects/p-1/runs/{run}/{what}");
    let bearer = Some("Bearer tok");

    // Off by default: the routes do not exist, and nothing is asked.
    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "pause"), bearer).0, 404);
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "pause"), None).0, 401);
    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "pause"), Some("Bearer wrong")).0, 401);

    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "pause"), bearer), (202, json!({"run": "r-ok"})));
    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "resume"), bearer), (202, json!({"run": "r-new"})));
    assert_eq!(post(&roots, &fake, "POST", &url("r-done", "pause"), bearer), (409, json!({"error": "r-done is completed; only a running run can be paused"})));
    assert_eq!(post(&roots, &fake, "POST", &url("r-broken", "resume"), bearer), (500, json!({"error": "the run could not be resumed"})));
    assert_eq!(post(&roots, &fake, "POST", &url("r-none", "pause"), bearer).0, 404);
    assert_eq!(post(&roots, &fake, "POST", "/api/projects/p-9/runs/r-ok/pause", bearer).0, 404);
    assert_eq!(post(&roots, &fake, "POST", &url("..", "pause"), bearer).0, 404);
    assert_eq!(post(&roots, &fake, "POST", &url("r-ok", "stop"), bearer).0, 405, "no other action exists");
    assert_eq!(
        *fake.asked.lock().unwrap(),
        [("p-1".to_string(), "r-ok".to_string(), RunControl::Pause), ("p-1".to_string(), "r-ok".to_string(), RunControl::Resume)]
    );

    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(post(&roots, &fake, method, &url("r-ok", "pause"), bearer).0, 405, "{method}");
    }

    let log = remote_audit::read(&roots, 20).unwrap();
    let seen: Vec<(String, String, String)> = log
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect();
    let row = |a: &str, o: &str, r: &str| (a.to_string(), o.to_string(), r.to_string());
    assert_eq!(
        seen,
        [
            row("run.pause", "refused", "remote actions are off"),
            row("run.pause", "allowed", ""),
            row("run.resume", "allowed", ""),
            row("run.pause", "refused", "r-done is completed; only a running run can be paused"),
            row("run.resume", "refused", "the run could not be resumed"),
            row("run.pause", "refused", "no such project or run"),
            row("run.pause", "refused", "no such project or run"),
            row("run.pause", "refused", "not a run id"),
        ]
    );
    assert_eq!(log.iter().rev().nth(1).unwrap()["run"], "r-ok");
}
