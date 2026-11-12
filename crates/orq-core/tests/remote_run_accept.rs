//! `POST /api/projects/{id}/runs/{run}/accept` asks the provider to accept a failed or needs_review run, the way
//! the popup's Accept does: its tests run in its worktree and it is review ready if they pass. It is there only
//! when remote actions are on and answers 202 once the tests have started. A refusal is the orchestrator's own
//! words; every attempt is audited as run.accept.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunControl, RunControlOutcome};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    asked: Mutex<Vec<(String, RunControl)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn control_run(&self, _id: &str, run: &str, control: RunControl) -> Result<RunControlOutcome, String> {
        self.asked.lock().unwrap().push((run.into(), control));
        match run {
            "r-ok" => Ok(RunControlOutcome::Done(run.into())),
            "r-run" => Ok(RunControlOutcome::Refused("r-run is running; only a failed run can be accepted, or one waiting on a review decision".into())),
            "r-broken" => Err("git rev-parse HEAD failed in /home/me/r-broken".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runaccept-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, run: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/p-1/runs/{run}/accept");
    let request = Request { method, path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, fake);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 20)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

#[test]
fn a_run_is_accepted_only_with_actions_on_and_every_attempt_is_audited_as_run_accept() {
    let roots = roots("accept");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    assert_eq!(send(&roots, &fake, "POST", "r-ok").0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "POST", "r-ok"), (202, json!({"run": "r-ok"})));
    assert_eq!(send(&roots, &fake, "POST", "r-run"), (409, json!({"error": "r-run is running; only a failed run can be accepted, or one waiting on a review decision"})));
    assert_eq!(send(&roots, &fake, "POST", "r-broken"), (500, json!({"error": "the run could not be accepted"})));
    assert_eq!(send(&roots, &fake, "POST", "r-none").0, 404);
    assert_eq!(fake.asked.lock().unwrap()[0], ("r-ok".to_string(), RunControl::Accept));

    let row = |o: &str, r: &str| ("run.accept".to_string(), o.to_string(), r.to_string());
    assert_eq!(
        audit(&roots),
        [
            row("refused", "remote actions are off"),
            row("allowed", ""),
            row("refused", "r-run is running; only a failed run can be accepted, or one waiting on a review decision"),
            row("refused", "the run could not be accepted"),
            row("refused", "no such project or run"),
        ]
    );
}

#[test]
fn only_a_post_accepts_a_run() {
    let roots = roots("methods");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    for method in ["GET", "PUT", "PATCH", "DELETE"] {
        send(&roots, &fake, method, "r-ok");
    }
    assert!(fake.asked.lock().unwrap().is_empty());
}
