//! `POST /api/projects/{id}/runs/{run}/build-report` asks the provider to build the review report an ended run could
//! not build. It is there only when remote actions are on; every attempt is audited as run.build_report.

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
            "r-has" => Ok(RunControlOutcome::Refused("this run's review report is not missing".into())),
            "r-broken" => Err("cannot open /home/me/.local/share/orq/orq.db".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots() -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-buildreport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, run: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/p-1/runs/{run}/build-report");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, fake);
    (r.status, r.body)
}

#[test]
fn a_missing_report_is_built_only_with_actions_on_and_every_attempt_is_audited() {
    let roots = roots();
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    assert_eq!(send(&roots, &fake, "r-ok").0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "r-ok"), (202, json!({"run": "r-ok"})));
    assert_eq!(send(&roots, &fake, "r-has"), (409, json!({"error": "this run's review report is not missing"})));
    assert_eq!(send(&roots, &fake, "r-broken"), (500, json!({"error": "the review report could not be built"})));
    assert_eq!(fake.asked.lock().unwrap()[0], ("r-ok".to_string(), RunControl::BuildReport));

    let actions: Vec<String> = remote_audit::read(&roots, 20).unwrap().iter().map(|l| l["action"].as_str().unwrap().to_string()).collect();
    assert_eq!(actions, vec!["run.build_report"; 4]);
}
