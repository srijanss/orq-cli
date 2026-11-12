//! The features and runs lists say whether remote actions are on, so the page draws its buttons from
//! the answer it already has instead of asking again.

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(Some(json!([{"name": "a", "status": "pending"}])))
    }

    fn runs(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(Some(json!([{"run_id": "r-1", "state": "running"}])))
    }
}

fn get(roots: &StorageRoots, path: &str) -> Value {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, &Fake);
    assert_eq!(r.status, 200, "{path}");
    r.body
}

#[test]
fn the_features_and_runs_lists_say_whether_remote_actions_are_on() {
    let base = orq_test_support::process_temp_dir().join(format!("orq-actions-flag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();

    for path in ["/api/projects/p-1/features", "/api/projects/p-1/runs"] {
        assert_eq!(get(&roots, path)["remote_actions"], json!(false), "{path}: off by default");
    }
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    for path in ["/api/projects/p-1/features", "/api/projects/p-1/runs"] {
        assert_eq!(get(&roots, path)["remote_actions"], json!(true), "{path}: switched on");
    }
    // The rows themselves are untouched.
    assert_eq!(get(&roots, "/api/projects/p-1/features")["features"], json!([{"name": "a", "status": "pending"}]));
}
