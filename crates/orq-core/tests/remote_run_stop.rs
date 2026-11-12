//! `POST /api/projects/{id}/runs/{run}/stop` asks the provider to stop the run, the way the popup's Stop does,
//! only when remote actions are on. A refusal is the orchestrator's own words; every attempt is audited as
//! run.stop.

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
            "r-done" => Ok(RunControlOutcome::Refused("r-done is failed; only a running run can be stopped".into())),
            "r-broken" => Err("the owner of r-broken (pid 4242) is still running".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runstop-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, run: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/p-1/runs/{run}/stop");
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
fn a_run_is_stopped_only_with_actions_on_and_every_attempt_is_audited_as_run_stop() {
    let roots = roots("stop");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    assert_eq!(send(&roots, &fake, "POST", "r-ok").0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "POST", "r-ok"), (202, json!({"run": "r-ok"})));
    assert_eq!(send(&roots, &fake, "POST", "r-done"), (409, json!({"error": "r-done is failed; only a running run can be stopped"})));
    assert_eq!(send(&roots, &fake, "POST", "r-broken"), (500, json!({"error": "the run could not be stopped"})));
    assert_eq!(send(&roots, &fake, "POST", "r-none").0, 404);
    assert_eq!(fake.asked.lock().unwrap()[0], ("r-ok".to_string(), RunControl::Stop));

    let row = |o: &str, r: &str| ("run.stop".to_string(), o.to_string(), r.to_string());
    assert_eq!(
        audit(&roots),
        [
            row("refused", "remote actions are off"),
            row("allowed", ""),
            row("refused", "r-done is failed; only a running run can be stopped"),
            row("refused", "the run could not be stopped"),
            row("refused", "no such project or run"),
        ]
    );
}

#[test]
fn only_a_post_stops_a_run() {
    let roots = roots("methods");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    for method in ["GET", "PUT", "PATCH", "DELETE"] {
        send(&roots, &fake, method, "r-ok");
    }
    assert!(fake.asked.lock().unwrap().is_empty());
}
