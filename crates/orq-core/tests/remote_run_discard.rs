//! `GET /api/projects/{id}/runs/{run}/discard-warning` says what a discard would take with the branch, and
//! `POST /api/projects/{id}/runs/{run}/discard` discards the run the way the popup's Discard and Clear do,
//! only when remote actions are on. A refusal is the orchestrator's own words; every discard attempt is
//! audited as run.discard, or run.clear when the run's work was on its target branch.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunControlOutcome, RunDiscard};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    asked: Mutex<Vec<String>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn discard_warning(&self, _id: &str, run: &str) -> Result<RunControlOutcome, String> {
        match run {
            "r-ok" => Ok(RunControlOutcome::Done("its branch orq/r-ok goes with 2 commits not on the target branch".into())),
            "r-merged" => Ok(RunControlOutcome::Done(String::new())),
            "r-broken" => Err("cannot read /home/someone/state".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }

    fn discard_run(&self, _id: &str, run: &str) -> Result<RunDiscard, String> {
        self.asked.lock().unwrap().push(run.into());
        match run {
            "r-ok" => Ok(RunDiscard::Discarded { note: " f-r-ok is pending again.".into(), cleared: false }),
            "r-merged" => Ok(RunDiscard::Discarded { note: String::new(), cleared: true }),
            "r-live" => Ok(RunDiscard::Refused("r-live is running; only a failed, blocked, paused or review-waiting run can be discarded".into())),
            "r-broken" => Err("git would not remove /home/someone/wt".into()),
            _ => Ok(RunDiscard::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-rundiscard-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, path: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
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
fn a_run_is_discarded_or_cleared_only_with_actions_on_and_every_attempt_is_audited() {
    let roots = roots("discard");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    let path = |run: &str| format!("/api/projects/p-1/runs/{run}/discard");
    assert_eq!(send(&roots, &fake, "POST", &path("r-ok")).0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "POST", &path("r-ok")), (202, json!({"run": "r-ok", "note": "f-r-ok is pending again.", "cleared": false})));
    assert_eq!(send(&roots, &fake, "POST", &path("r-merged")), (202, json!({"run": "r-merged", "note": "", "cleared": true})));
    let (status, body) = send(&roots, &fake, "POST", &path("r-live"));
    assert_eq!((status, body["error"].as_str().unwrap().starts_with("r-live is running")), (409, true), "{body}");
    assert_eq!(send(&roots, &fake, "POST", &path("r-broken")), (500, json!({"error": "the run could not be discarded"})));
    assert_eq!(send(&roots, &fake, "POST", &path("r-none")).0, 404);

    let row = |a: &str, o: &str, r: &str| (a.to_string(), o.to_string(), r.to_string());
    assert_eq!(
        audit(&roots),
        [
            row("run.discard", "refused", "remote actions are off"),
            row("run.discard", "allowed", ""),
            row("run.clear", "allowed", ""),
            row("run.discard", "refused", "r-live is running; only a failed, blocked, paused or review-waiting run can be discarded"),
            row("run.discard", "refused", "the run could not be discarded"),
            row("run.discard", "refused", "no such project or run"),
        ]
    );
}

#[test]
fn the_discard_warning_is_there_only_with_actions_on_and_is_not_audited() {
    let roots = roots("warning");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    let path = |run: &str| format!("/api/projects/p-1/runs/{run}/discard-warning");
    assert_eq!(send(&roots, &fake, "GET", &path("r-ok")).0, 404, "off by default");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "GET", &path("r-ok")), (200, json!({"run": "r-ok", "warning": "its branch orq/r-ok goes with 2 commits not on the target branch"})));
    assert_eq!(send(&roots, &fake, "GET", &path("r-merged")), (200, json!({"run": "r-merged", "warning": ""})));
    assert_eq!(send(&roots, &fake, "GET", &path("r-broken")), (500, json!({"error": "the run could not be checked"})));
    assert_eq!(send(&roots, &fake, "GET", &path("r-none")).0, 404);
    assert!(fake.asked.lock().unwrap().is_empty(), "a warning discards nothing");
    assert!(audit(&roots).is_empty());
}

#[test]
fn only_a_post_discards_a_run() {
    let roots = roots("methods");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    for method in ["GET", "PUT", "PATCH", "DELETE"] {
        send(&roots, &fake, method, "/api/projects/p-1/runs/r-ok/discard");
    }
    assert!(fake.asked.lock().unwrap().is_empty());
}
