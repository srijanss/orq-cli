//! A start from the web may choose the implementer and reviewer, as the popup's launch picker does. Only
//! profiles the picker lists are accepted, the CLI's eligibility rule still comes first, and the launcher
//! is handed the chosen profiles instead of the defaults.

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
    started: Mutex<Vec<(String, Option<LaunchProfiles>)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, feature: &str) -> Result<String, LaunchError> {
        self.started.lock().unwrap().push((feature.to_string(), None));
        Ok("run-default".into())
    }

    fn start_with(&self, _repo: &Path, feature: &str, profiles: &LaunchProfiles) -> Result<String, LaunchError> {
        self.started.lock().unwrap().push((feature.to_string(), Some(profiles.clone())));
        Ok("run-chosen".into())
    }
}

/// A launcher that can only start with the defaults.
struct DefaultsOnly;

impl RunLauncher for DefaultsOnly {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Ok("run-default".into())
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
        {"featureName": "ready", "dependsOn": [], "status": "pending"},
        {"featureName": "idea", "dependsOn": [], "status": "draft"}
    ]);
    std::fs::write(path.join(".tdd-features.json"), ledger.to_string()).unwrap();
    Setup { scan: base.join("code"), roots }
}

fn start(s: &Setup, launcher: Arc<dyn RunLauncher + Send + Sync>, feature: &str, body: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(launcher);
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/features/{feature}/runs");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

const CHOSEN: &str = r#"{"implementer": "opus", "reviewer": "sonnet"}"#;

#[test]
fn the_chosen_profiles_reach_the_launcher_only_when_the_picker_lists_them() {
    let s = setup("run-start-options");
    let launcher = Arc::new(Launcher::default());
    assert_eq!(start(&s, launcher.clone(), "ready", CHOSEN), (202, json!({"run": "run-chosen"})));
    assert_eq!(start(&s, launcher.clone(), "ready", "{}"), (202, json!({"run": "run-default"})));
    assert_eq!(
        start(&s, launcher.clone(), "ready", r#"{"implementer": "gpt", "reviewer": "sonnet"}"#),
        (409, json!({"error": "the profile gpt is not defined in the model profiles"}))
    );
    assert_eq!(
        start(&s, launcher.clone(), "ready", r#"{"implementer": "opus", "reviewer": "haiku"}"#),
        (409, json!({"error": "the profile haiku is not defined in the model profiles"}))
    );
    assert_eq!(start(&s, launcher.clone(), "idea", CHOSEN), (409, json!({"error": "feature is draft, only a pending feature can start a run"})));
    assert_eq!(start(&s, launcher.clone(), "nothing-like-it", CHOSEN).0, 404);
    let chosen = LaunchProfiles { implementer: "opus".into(), reviewer: "sonnet".into() };
    assert_eq!(*launcher.started.lock().unwrap(), [("ready".to_string(), Some(chosen)), ("ready".to_string(), None)]);

    // A launcher that can only use the defaults says so instead of ignoring the choice.
    assert_eq!(start(&s, Arc::new(DefaultsOnly), "ready", CHOSEN), (409, json!({"error": "this server cannot choose the profiles"})));
}
