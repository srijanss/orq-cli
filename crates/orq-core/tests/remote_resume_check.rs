//! GET /api/projects/{id}/runs/{run}/resume-check says where a run would continue, or why it cannot,
//! before the web asks "Resume {run} from {next}?". Only while remote actions are on.

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request, RunControlOutcome};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn resume_check(&self, id: &str, run: &str) -> Result<RunControlOutcome, String> {
        match (id, run) {
            ("p-1", "r-paused") => Ok(RunControlOutcome::Done("review".into())),
            ("p-1", "r-done") => Ok(RunControlOutcome::Refused("r-done is completed; only a paused run can be resumed".into())),
            ("p-1", "r-broken") => Err("cannot read /home/someone/.local/state/orq/runs/r-broken".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(label: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-resume-check-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn get(roots: &StorageRoots, method: &str, path: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, &Fake);
    (r.status, r.body)
}

#[test]
fn the_check_says_where_the_run_would_continue_or_why_not() {
    let roots = roots("answers");
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/runs/r-paused/resume-check").0, 404, "off unless remote actions are on");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/runs/r-paused/resume-check"), (200, json!({"run": "r-paused", "next": "review"})));
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/runs/r-done/resume-check"), (409, json!({"error": "r-done is completed; only a paused run can be resumed"})));
    let (status, body) = get(&roots, "GET", "/api/projects/p-1/runs/r-broken/resume-check");
    assert_eq!((status, body["error"].clone()), (500, json!("the run could not be checked")), "a machine path never reaches the page");
    assert_eq!(get(&roots, "GET", "/api/projects/p-2/runs/r-paused/resume-check").0, 404);
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/runs/..%2f/resume-check").0, 404);
    assert_eq!(get(&roots, "POST", "/api/projects/p-1/runs/r-paused/resume-check").0, 405, "it only reads");
    assert!(orq_core::remote_audit::read(&roots, 50).unwrap().is_empty(), "a read is not an action, so nothing is audited");
}
