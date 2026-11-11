//! Pausing and resuming a run from the web: pause goes through the orchestrator's own request, resume through the launcher.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

#[derive(Default)]
struct Launcher {
    resumed: Mutex<Vec<(PathBuf, String)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Failed("not used".into()))
    }

    fn resume(&self, repo: &Path, run: &str) -> Result<String, LaunchError> {
        self.resumed.lock().unwrap().push((repo.to_path_buf(), run.to_string()));
        match run {
            "r-stale" => Err(LaunchError::Refused("the checkpoint no longer matches the checkout".into())),
            "r-crash" => Err(LaunchError::Failed("cannot find /usr/local/bin/orq".into())),
            _ => Ok("run-2".into()),
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

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

fn add_run(s: &Setup, run: &str, project: &str, kinds: &[&str]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default() })
        .unwrap();
    for (n, kind) in kinds.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: None, attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload: json!({}) };
        store.append_event(&event).unwrap();
    }
}

fn post(s: &Setup, launcher: Option<&Arc<Launcher>>, path: &str) -> (u16, Value) {
    let mut provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    if let Some(launcher) = launcher {
        provider = provider.with_launcher(launcher.clone());
    }
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

fn kinds(s: &Setup, run: &str) -> Vec<String> {
    RunStore::new(&s.roots.state).events(run).unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn a_running_run_is_asked_to_pause_and_any_other_run_is_refused_with_the_orchestrators_words() {
    let s = setup("pause");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, "r-run", &project_id(&alpha), &["run.started"]);
    add_run(&s, "r-done", &project_id(&alpha), &["run.started", "run.failed"]);
    add_run(&s, "r-other", &project_id(&beta), &["run.started"]);
    let pause = |run: &str| post(&s, None, &format!("/api/projects/alpha/runs/{run}/pause"));

    assert_eq!(pause("r-run"), (202, json!({"run": "r-run"})));
    assert_eq!(kinds(&s, "r-run"), ["run.started", "run.pause_requested"]);
    assert_eq!(pause("r-run"), (409, json!({"error": "r-run is already pause_requested"})));
    assert_eq!(kinds(&s, "r-run").len(), 2, "a second request changes nothing");
    assert_eq!(pause("r-done"), (409, json!({"error": "r-done is failed; only a running run can be paused"})));
    assert_eq!(kinds(&s, "r-done"), ["run.started", "run.failed"]);

    // A run of another project, or none at all, is not found, and nothing is written.
    assert_eq!(pause("r-other").0, 404);
    assert_eq!(pause("r-nothing").0, 404);
    assert_eq!(kinds(&s, "r-other"), ["run.started"]);
    assert_eq!(post(&s, None, "/api/projects/gamma/runs/r-run/pause").0, 404);
}

#[test]
fn a_resume_goes_to_the_launcher_for_a_run_of_the_project_and_its_refusal_is_the_reason() {
    let s = setup("resume");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    for run in ["r-ok", "r-stale", "r-crash"] {
        add_run(&s, run, &project_id(&alpha), &["run.started", "run.paused"]);
    }
    add_run(&s, "r-other", &project_id(&beta), &["run.started"]);
    let launcher = Arc::new(Launcher::default());
    let resume = |run: &str| post(&s, Some(&launcher), &format!("/api/projects/alpha/runs/{run}/resume"));

    assert_eq!(resume("r-ok"), (202, json!({"run": "run-2"})));
    assert_eq!(*launcher.resumed.lock().unwrap(), [(alpha.clone(), "r-ok".to_string())]);
    assert_eq!(resume("r-stale"), (409, json!({"error": "the checkpoint no longer matches the checkout"})));
    assert_eq!(resume("r-crash"), (500, json!({"error": "the run could not be resumed"})));
    assert_eq!(resume("r-other").0, 404);
    assert_eq!(resume("r-nothing").0, 404);
    assert_eq!(launcher.resumed.lock().unwrap().len(), 3, "a run that is not this project's never reaches the launcher");

    // A server with no launcher says so instead of pretending.
    assert_eq!(post(&s, None, "/api/projects/alpha/runs/r-ok/resume"), (409, json!({"error": "this server cannot resume runs"})));
}

#[test]
fn a_running_run_is_stopped_through_the_orchestrator_and_any_other_run_is_refused_with_its_words() {
    let s = setup("stop");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, "r-run", &project_id(&alpha), &["run.started"]);
    add_run(&s, "r-asked", &project_id(&alpha), &["run.started", "run.pause_requested"]);
    add_run(&s, "r-done", &project_id(&alpha), &["run.started", "run.failed"]);
    add_run(&s, "r-other", &project_id(&beta), &["run.started"]);
    let stop = |run: &str| post(&s, None, &format!("/api/projects/alpha/runs/{run}/stop"));

    assert_eq!(stop("r-run"), (202, json!({"run": "r-run"})));
    assert_eq!(kinds(&s, "r-run"), ["run.started", "run.failed"]);
    assert_eq!(stop("r-asked"), (202, json!({"run": "r-asked"})));
    assert_eq!(stop("r-done"), (409, json!({"error": "r-done is failed; only a running run can be stopped"})));
    assert_eq!(kinds(&s, "r-done"), ["run.started", "run.failed"]);
    assert_eq!(stop("r-other").0, 404);
    assert_eq!(kinds(&s, "r-other"), ["run.started"], "another project's run is untouched");
}
