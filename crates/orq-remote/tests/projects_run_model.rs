//! Switching a paused run's model from the web goes through the orchestrator's own request, the way the
//! popup's Switch model does: only to a profile the popup's picker offers, and only for a paused run.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
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
    std::fs::write(roots.config_file(), "remote_actions = true\n\n[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n").unwrap();
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

fn switch(s: &Setup, project: &str, run: &str, body: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/{project}/runs/{run}/model");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

fn events(s: &Setup, run: &str) -> Vec<(String, Value)> {
    RunStore::new(&s.roots.state).events(run).unwrap().into_iter().map(|e| (e.kind, e.payload)).collect()
}

#[test]
fn a_paused_run_is_asked_to_switch_to_a_known_profile_and_anything_else_is_refused_in_the_orchestrators_words() {
    let s = setup("run-model");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    std::fs::write(alpha.join(".orq.toml"), "[models.astra]\nharness = \"pi\"\nmodel = \"openai/astra\"\n").unwrap();
    add_run(&s, "r-paused", &project_id(&alpha), &["run.started", "run.pause_requested", "run.paused"]);
    add_run(&s, "r-run", &project_id(&alpha), &["run.started"]);
    add_run(&s, "r-other", &project_id(&beta), &["run.started", "run.pause_requested", "run.paused"]);

    assert_eq!(switch(&s, "alpha", "r-paused", r#"{"role":"reviewer","profile":"astra"}"#), (202, json!({"run": "r-paused", "role": "reviewer", "profile": "astra"})));
    assert_eq!(events(&s, "r-paused").last().unwrap(), &("run.model_switch_requested".to_string(), json!({"role": "reviewer", "profile": "astra"})));

    assert_eq!(switch(&s, "alpha", "r-paused", r#"{"role":"implementer","profile":"nope"}"#), (409, json!({"error": "the profile nope is not defined in the model profiles"})));
    assert_eq!(switch(&s, "alpha", "r-run", r#"{"role":"implementer","profile":"opus"}"#), (409, json!({"error": "r-run is running; only a paused run can switch models"})));
    assert_eq!(events(&s, "r-run").len(), 1, "a refused switch records nothing");

    // A run of another project, or none at all, is not found, and nothing is written.
    assert_eq!(switch(&s, "alpha", "r-other", r#"{"role":"implementer","profile":"opus"}"#).0, 404);
    assert_eq!(switch(&s, "alpha", "r-nothing", r#"{"role":"implementer","profile":"opus"}"#).0, 404);
    assert_eq!(events(&s, "r-other").len(), 3);
    assert_eq!(events(&s, "r-paused").len(), 4);
}
