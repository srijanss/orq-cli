//! A run's timeline says whether remote actions are on, so the run detail can offer the run's actions.

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn run_timeline(&self, _id: &str, _run: &str, _after: Option<&str>) -> Result<Option<Value>, String> {
        Ok(Some(json!({"run": {"run_id": "r-1"}, "events": [], "next_cursor": null})))
    }
}

fn roots() -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-timeline-actions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

#[test]
fn the_timeline_carries_remote_actions() {
    let roots = roots();
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path: "/api/projects/p-1/runs/r-1", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let read = |roots: &StorageRoots| handle_with(roots, &config, "p", "p", &request, &Fake).body;
    assert_eq!(read(&roots)["remote_actions"], json!(false));
    assert_eq!(read(&roots)["run"]["run_id"], "r-1");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    assert_eq!(read(&roots)["remote_actions"], json!(true));
}
