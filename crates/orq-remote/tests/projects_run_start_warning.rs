//! A start the launcher has a warning about, such as the local target branch having commits its remote does
//! not, still starts, and the reply carries the warning so the page can show it. A start without one says nothing more.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

const WARNING: &str = "main has 2 commits origin/main does not; the run starts from origin/main without them";

struct Launcher {
    warning: Option<String>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Ok("run-9".into())
    }

    fn start_warning(&self, _repo: &Path) -> Option<String> {
        self.warning.clone()
    }
}

fn setup(label: &str) -> (PathBuf, StorageRoots) {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    std::fs::write(path.join(".tdd-features.json"), json!([{"featureName": "ready", "dependsOn": [], "status": "pending"}]).to_string()).unwrap();
    (scan, roots)
}

fn start(label: &str, warning: Option<&str>) -> (u16, Value) {
    let (scan, roots) = setup(label);
    let launcher = Arc::new(Launcher { warning: warning.map(str::to_string) });
    let provider = StoreProjects::new(roots.clone(), scan).with_launcher(launcher);
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/projects/alpha/features/ready/runs", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn a_start_with_a_warning_replies_with_the_run_and_the_warning() {
    assert_eq!(start("warn-start", Some(WARNING)), (202, json!({"run": "run-9", "warning": WARNING})));
}

#[test]
fn a_start_without_a_warning_replies_with_the_run_alone() {
    assert_eq!(start("plain-start", None), (202, json!({"run": "run-9"})));
}

/// The warning is the launcher's own text, so a secret in it, such as a token in a remote's URL, is kept
/// from the web like any other; the branch names it is about stay readable.
#[test]
fn a_warning_never_shows_a_secret() {
    let (status, body) = start("secret-warning", Some("origin/main is https://me:ghp_abcdefghijklmnopqrstuvwxyz0123@example.invalid/r"));
    assert_eq!(status, 202);
    let shown = body["warning"].as_str().unwrap();
    assert!(!shown.contains("ghp_abc"), "{body}");
    assert!(shown.starts_with("origin/main is "), "{body}");
}
