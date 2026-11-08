//! The real provider maps a discovered project's id to the key its reviews are stored under, so the
//! scoped review routes show that project's reports and no one else's.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::projects::StoreProjects;

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn id_of(path: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(path).unwrap())
}

#[test]
fn a_discovered_project_serves_its_own_reviews_and_an_unknown_id_is_not_found() {
    let base = web_deps::private_temp_dir("review-scope");
    let scan = base.join("code");
    let alpha = git_repo(&scan, "alpha");
    let beta = git_repo(&scan, "beta");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let report = serde_json::to_vec(&serde_json::json!({"status": "current", "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []})).unwrap();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-alpha", &id_of(&alpha), "fa", &"a".repeat(40), &report).unwrap();
    store.save_review(&roots.data, "run-beta", &id_of(&beta), "fb", &"b".repeat(40), &report).unwrap();

    let provider = StoreProjects::new(roots.clone(), scan);
    let config = RemoteConfig { token: "tok".into() };
    let ask = |path: &str| {
        let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let r = handle_with(&roots, &config, "server-project", "server", &request, &provider);
        (r.status, r.body)
    };

    let (status, body) = ask("/api/projects/beta/reviews");
    assert_eq!(status, 200, "{body}");
    let rows = body["reviews"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!((rows[0]["run_id"].as_str(), rows[0]["project"].as_str()), (Some("run-beta"), Some("beta")));
    assert_eq!(ask("/api/projects/beta/reviews/run-beta").0, 200);
    assert_eq!(ask("/api/projects/beta/reviews/run-alpha").0, 404);
    assert_eq!(ask("/api/projects/alpha/reviews/run-alpha").0, 200);
    assert_eq!(ask("/api/projects/gamma/reviews").0, 404);
    assert!(provider.review_scope("../etc").unwrap().is_none());
}
