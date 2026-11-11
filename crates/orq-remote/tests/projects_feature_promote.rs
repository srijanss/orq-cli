//! Promoting a feature from the web goes through the ledger's own promote, the way the popup's Promote does:
//! a draft becomes pending under the ledger's lock, and anything else is refused in the CLI's words.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn promote(s: &Setup, project: &str, feature: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/{project}/features/{feature}/promote");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, b"", &provider);
    (r.status, r.body)
}

fn statuses(repo: &Path) -> Vec<(String, String)> {
    let ledger: Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".tdd-features.json")).unwrap()).unwrap();
    ledger.as_array().unwrap().iter().map(|e| (e["featureName"].as_str().unwrap().to_string(), e["status"].as_str().unwrap().to_string())).collect()
}

#[test]
fn a_draft_becomes_pending_and_any_other_feature_is_refused_in_the_clis_words() {
    let s = setup("promote");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    std::fs::write(alpha.join(".tdd-features.json"), r#"[{"featureName":"f-draft","status":"draft"},{"featureName":"f-done","status":"completed"},{"featureName":"f-odd","status":"someday"}]"#).unwrap();

    assert_eq!(promote(&s, "alpha", "f-draft"), (200, json!({"feature": "f-draft", "status": "pending"})));
    assert_eq!(statuses(&alpha)[0], ("f-draft".to_string(), "pending".to_string()));
    assert_eq!(promote(&s, "alpha", "f-draft"), (409, json!({"error": "feature f-draft: Pending to Pending is not allowed"})));
    assert_eq!(promote(&s, "alpha", "f-done"), (409, json!({"error": "feature f-done: Completed to Pending is not allowed"})));
    assert_eq!(promote(&s, "alpha", "f-odd"), (409, json!({"error": "feature f-odd has unrecognised status \"someday\" and is left alone"})));
    assert_eq!(statuses(&alpha)[1..], [("f-done".to_string(), "completed".to_string()), ("f-odd".to_string(), "someday".to_string())]);

    // A feature, project or ledger that is not there is not found.
    assert_eq!(promote(&s, "alpha", "f-none").0, 404);
    assert_eq!(promote(&s, "beta", "f-draft").0, 404, "beta has no ledger");
    assert_eq!(promote(&s, "gamma", "f-draft").0, 404);
    assert!(!beta.join(".tdd-features.json").exists());

    // A ledger that cannot be read is this machine's problem; the caller is only told it failed.
    std::fs::write(beta.join(".tdd-features.json"), "not json").unwrap();
    assert_eq!(promote(&s, "beta", "f-draft"), (500, json!({"error": "the feature could not be promoted"})));
}
