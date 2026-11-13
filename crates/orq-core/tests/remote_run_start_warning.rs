//! A run that started with a warning about it is replied to with the run and the warning, and audited as allowed.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, _feature: &str) -> Result<RunStart, String> {
        Ok(RunStart::StartedWithWarning { run: "run-7".into(), warning: "main has 1 commit origin/main does not".into() })
    }
}

#[test]
fn a_warned_start_replies_with_the_run_and_the_warning() {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runstart-warning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/projects/p-1/features/ready/runs", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };

    let r = handle_with(&roots, &config, "p", "p", &request, &Fake);
    assert_eq!((r.status, r.body), (202, json!({"run": "run-7", "warning": "main has 1 commit origin/main does not"})));
    let log = remote_audit::read(&roots, 5).unwrap();
    assert_eq!(log[0]["outcome"], "allowed", "{log:?}");
    assert_eq!(log[0]["run"], "run-7", "{log:?}");
}
