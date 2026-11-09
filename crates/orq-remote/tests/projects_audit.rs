//! `/api/audit`: the remote action log, readable behind the same checks and never writable from the web.

mod web_deps;

use orq_core::remote_audit::{self, Attempt, Outcome};
use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::Value;

fn setup(label: &str) -> (StoreProjects, StorageRoots) {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    (StoreProjects::new(roots.clone(), base.join("code")), roots)
}

fn ask(s: &(StoreProjects, StorageRoots), method: &str, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with(&s.1, &config, "p", "p", &request, &s.0);
    (r.status, r.body)
}

#[test]
fn the_audit_log_is_listed_newest_first_behind_the_token_and_cannot_be_changed_from_the_web() {
    let s = setup("audit-list");
    assert_eq!(ask(&s, "GET", "/api/audit", Some("Bearer tok")), (200, serde_json::json!({"audit": []})));
    let attempt = |action| Attempt { identity: "phone", action, project: "p-1", run: None, outcome: Outcome::Allowed, reason: None };
    remote_audit::record(&s.1, &attempt("run.start"), 10).unwrap();
    remote_audit::record(&s.1, &attempt("run.pause"), 11).unwrap();
    let (status, body) = ask(&s, "GET", "/api/audit", Some("Bearer tok"));
    assert_eq!(status, 200);
    let actions: Vec<&str> = body["audit"].as_array().unwrap().iter().map(|line| line["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["run.pause", "run.start"]);

    assert_eq!(ask(&s, "GET", "/api/audit", None).0, 401);
    assert_eq!(ask(&s, "GET", "/api/audit", Some("Bearer wrong")).0, 401);
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        assert_eq!(ask(&s, method, "/api/audit", Some("Bearer tok")).0, 405, "{method}");
    }
    assert_eq!(remote_audit::read(&s.1, 10).unwrap().len(), 2, "the log is as it was");
}
