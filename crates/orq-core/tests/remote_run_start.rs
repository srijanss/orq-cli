//! `POST /api/projects/{id}/features/{name}/runs`: starts a run only when remote actions are switched on,
//! says why when it will not, and leaves one audit line for every attempt that got past the token.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    started: Mutex<Vec<(String, String)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, id: &str, feature: &str) -> Result<RunStart, String> {
        match (id, feature) {
            ("p-1", "ready") => {
                self.started.lock().unwrap().push((id.into(), feature.into()));
                Ok(RunStart::Started("run-7".into()))
            }
            ("p-1", "draft") => Ok(RunStart::Refused("the feature is still a draft".into())),
            ("p-1", "broken") => Err("/home/me/secret/path exploded".into()),
            _ => Ok(RunStart::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runstart-{}-{tag}", std::process::id()));
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
fn a_run_starts_only_when_actions_are_on_and_every_attempt_is_audited() {
    let roots = roots("start");
    let fake = Fake { started: Mutex::new(Vec::new()) };
    let url = |feature: &str| format!("/api/projects/p-1/features/{feature}/runs");
    let bearer = Some("Bearer tok");

    // Off by default: the route does not exist, and nothing starts.
    assert_eq!(post(&roots, &fake, "POST", &url("ready"), bearer).0, 404);
    assert!(fake.started.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    // A stranger is turned away before anything is asked or written.
    assert_eq!(post(&roots, &fake, "POST", &url("ready"), None).0, 401);
    assert_eq!(post(&roots, &fake, "POST", &url("ready"), Some("Bearer wrong")).0, 401);

    assert_eq!(post(&roots, &fake, "POST", &url("ready"), bearer), (202, json!({"run": "run-7"})));
    assert_eq!(*fake.started.lock().unwrap(), [("p-1".to_string(), "ready".to_string())]);
    assert_eq!(post(&roots, &fake, "POST", &url("draft"), bearer), (409, json!({"error": "the feature is still a draft"})));
    // An unexpected failure never shows the machine's paths.
    assert_eq!(post(&roots, &fake, "POST", &url("broken"), bearer), (500, json!({"error": "the run could not be started"})));
    assert_eq!(post(&roots, &fake, "POST", "/api/projects/p-9/features/ready/runs", bearer).0, 404);
    assert_eq!(post(&roots, &fake, "POST", "/api/projects/p-1/features/../runs", bearer).0, 404);
    assert_eq!(fake.started.lock().unwrap().len(), 1, "only the one ready feature started");

    // Other methods on the route stay read-only refusals.
    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(post(&roots, &fake, method, &url("ready"), bearer).0, 405, "{method}");
    }

    let log = remote_audit::read(&roots, 20).unwrap();
    let seen: Vec<(String, String)> = log.iter().rev().map(|l| (l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string())).collect();
    assert_eq!(
        seen,
        [
            ("refused".to_string(), "remote actions are off".to_string()),
            ("allowed".to_string(), String::new()),
            ("refused".to_string(), "the feature is still a draft".to_string()),
            ("refused".to_string(), "the run could not be started".to_string()),
            ("refused".to_string(), "no such project or feature".to_string()),
            ("refused".to_string(), "not a feature name".to_string()),
        ]
    );
    assert!(log.iter().all(|l| l["action"] == "run.start"), "{log:?}");
    assert_eq!(log.iter().rev().filter(|l| l["project"] == "p-9").count(), 1, "{log:?}");
    assert_eq!(log.iter().rev().nth(1).unwrap()["run"], "run-7");
}
