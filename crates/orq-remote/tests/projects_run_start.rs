//! Starting a run from the web asks the ledger first, with the CLI's own eligibility rule, and only then the launcher.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

#[derive(Default)]
struct Launcher {
    started: Mutex<Vec<(PathBuf, String)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        self.started.lock().unwrap().push((repo.to_path_buf(), feature.to_string()));
        match feature {
            "model-less" => Err(LaunchError::Refused("no implementer model is set".into())),
            "crashes" => Err(LaunchError::Failed("cannot find /usr/local/bin/orq".into())),
            _ => Ok("run-9".into()),
        }
    }
}

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

fn project(s: &Setup, name: &str, ledger: Value) -> PathBuf {
    let path = s.scan.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    std::fs::write(path.join(".tdd-features.json"), ledger.to_string()).unwrap();
    path
}

fn start(s: &Setup, launcher: Option<&Arc<Launcher>>, path: &str) -> (u16, Value) {
    let mut provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    if let Some(launcher) = launcher {
        provider = provider.with_launcher(launcher.clone());
    }
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn only_a_pending_feature_with_its_dependencies_met_reaches_the_launcher() {
    let s = setup("run-start");
    let repo = project(
        &s,
        "alpha",
        json!([
            {"featureName": "base", "dependsOn": [], "status": "completed"},
            {"featureName": "ready", "dependsOn": ["base"], "status": "pending"},
            {"featureName": "blocked", "dependsOn": ["later"], "status": "pending"},
            {"featureName": "later", "dependsOn": [], "status": "pending"},
            {"featureName": "idea", "dependsOn": [], "status": "draft"},
            {"featureName": "model-less", "dependsOn": [], "status": "pending"},
            {"featureName": "crashes", "dependsOn": [], "status": "pending"}
        ]),
    );
    let launcher = Arc::new(Launcher::default());
    let go = |feature: &str| start(&s, Some(&launcher), &format!("/api/projects/alpha/features/{feature}/runs"));

    assert_eq!(go("ready"), (202, json!({"run": "run-9"})));
    assert_eq!(*launcher.started.lock().unwrap(), [(repo.clone(), "ready".to_string())]);

    assert_eq!(go("blocked"), (409, json!({"error": "unmet dependencies: later (pending)"})));
    assert_eq!(go("idea"), (409, json!({"error": "feature is draft, only a pending feature can start a run"})));
    assert_eq!(go("base"), (409, json!({"error": "feature is completed, only a pending feature can start a run"})));
    assert_eq!(launcher.started.lock().unwrap().len(), 1, "an ineligible feature never reaches the launcher");

    assert_eq!(go("model-less"), (409, json!({"error": "no implementer model is set"})));
    assert_eq!(go("crashes"), (500, json!({"error": "the run could not be started"})));
    assert_eq!(go("nothing-like-it").0, 404);
    assert_eq!(start(&s, Some(&launcher), "/api/projects/beta/features/ready/runs").0, 404);

    // A server with no launcher says so instead of pretending.
    assert_eq!(start(&s, None, "/api/projects/alpha/features/ready/runs"), (409, json!({"error": "this server cannot start runs"})));
}
