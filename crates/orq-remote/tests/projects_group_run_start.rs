//! A group start from the web launches one group run of the marked features, in the ledger's dependsOn
//! order, with the chosen profiles or the defaults. The CLI's group rule comes first: every member must be
//! pending, and a dependency outside the group completed; a chosen profile must be one the picker lists.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with_body, LaunchProfiles, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

#[derive(Default)]
struct Launcher {
    groups: Mutex<Vec<(Vec<String>, Option<LaunchProfiles>)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        panic!("a group start is not a run of one feature")
    }

    fn start_group(&self, _repo: &Path, features: &[String], profiles: Option<&LaunchProfiles>) -> Result<String, LaunchError> {
        self.groups.lock().unwrap().push((features.to_vec(), profiles.cloned()));
        Ok("run-group".into())
    }

    fn start_warning(&self, _repo: &Path) -> Option<String> {
        Some("main is 1 commit ahead of origin/main".into())
    }
}

/// A launcher that cannot start a group run.
struct SinglesOnly;

impl RunLauncher for SinglesOnly {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Ok("run-single".into())
    }
}

const CONFIG: &str = "remote_actions = true\n\n[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n";

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), CONFIG).unwrap();
    let path = base.join("code").join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    let ledger = json!([
        {"featureName": "top", "dependsOn": ["base"], "status": "pending"},
        {"featureName": "base", "dependsOn": [], "status": "pending"},
        {"featureName": "waits", "dependsOn": ["elsewhere"], "status": "pending"},
        {"featureName": "elsewhere", "dependsOn": [], "status": "pending"},
        {"featureName": "idea", "dependsOn": [], "status": "draft"}
    ]);
    std::fs::write(path.join(".tdd-features.json"), ledger.to_string()).unwrap();
    Setup { scan: base.join("code"), roots }
}

fn start(s: &Setup, launcher: Option<Arc<dyn RunLauncher + Send + Sync>>, feature: &str, body: &str) -> (u16, Value) {
    let mut provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    if let Some(launcher) = launcher {
        provider = provider.with_launcher(launcher);
    }
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/features/{feature}/runs");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

const WARNED: &str = "main is 1 commit ahead of origin/main";

#[test]
fn a_group_start_launches_one_group_run_in_dependency_order_with_the_chosen_profiles_or_the_defaults() {
    let s = setup("group-run-start");
    let launcher = Arc::new(Launcher::default());
    assert_eq!(start(&s, Some(launcher.clone()), "top", r#"{"group": ["top", "base"]}"#), (202, json!({"run": "run-group", "warning": WARNED})));
    assert_eq!(
        start(&s, Some(launcher.clone()), "base", r#"{"group": ["base", "top"], "implementer": "opus", "reviewer": "sonnet"}"#),
        (202, json!({"run": "run-group", "warning": WARNED}))
    );
    let names = || vec!["base".to_string(), "top".to_string()];
    let chosen = LaunchProfiles { implementer: "opus".into(), reviewer: "sonnet".into() };
    assert_eq!(*launcher.groups.lock().unwrap(), [(names(), None), (names(), Some(chosen))]);
}

#[test]
fn a_group_the_cli_would_refuse_or_with_an_unlisted_profile_launches_nothing() {
    let s = setup("group-run-refused");
    let launcher = Arc::new(Launcher::default());
    let refused = |feature: &str, body: &str| start(&s, Some(launcher.clone()), feature, body);
    assert_eq!(refused("base", r#"{"group": ["base", "idea"]}"#), (409, json!({"error": "feature idea may not start a run: feature is draft, only a pending feature can start a run"})));
    assert_eq!(refused("base", r#"{"group": ["base", "waits"]}"#).0, 409);
    assert_eq!(refused("base", r#"{"group": ["base", "missing"]}"#), (409, json!({"error": "feature missing may not start a run: feature not found in the ledger"})));
    assert_eq!(
        refused("base", r#"{"group": ["base", "top"], "implementer": "gpt", "reviewer": "sonnet"}"#),
        (409, json!({"error": "the profile gpt is not defined in the model profiles"}))
    );
    assert!(launcher.groups.lock().unwrap().is_empty());
    assert_eq!(start(&s, None, "base", r#"{"group": ["base", "top"]}"#), (409, json!({"error": "this server cannot start runs"})));
    assert_eq!(start(&s, Some(Arc::new(SinglesOnly)), "base", r#"{"group": ["base", "top"]}"#), (409, json!({"error": "this server cannot start a group run"})));
}

#[test]
fn a_group_in_an_unknown_project_is_not_found() {
    let s = setup("group-run-unknown");
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(Arc::new(Launcher::default()));
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/projects/nope/features/base/runs", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, br#"{"group": ["base", "top"]}"#, &provider);
    assert_eq!(r.status, 404);
}
