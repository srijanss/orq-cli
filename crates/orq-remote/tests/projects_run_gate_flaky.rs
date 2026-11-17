//! A gate whose tests passed only on a retry says which, on the run page: a `gate.flaky_tests` event carries
//! each test's name and the retry it passed on, and nothing else of its payload.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
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
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn add_run(s: &Setup, run: &str, project: &str, events: &[(&str, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: Some("verify".into()), attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload: payload.clone() };
        store.append_event(&event).unwrap();
    }
}

fn ask(s: &Setup, path: &str) -> Value {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

#[test]
fn a_flaky_gate_event_lists_each_test_and_its_retry_and_no_other_event_has_them() {
    let s = setup("gate-flaky-shown");
    let alpha = git_repo(&s.scan, "alpha");
    let project = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    let flaky = json!({"tests": [
        {"name": "orq-cli::pipeline two_runs_in_one_second", "retry": 1, "log": "/home/someone/.local/state/orq/gate.log"},
        {"name": 7, "retry": 1},
        {"name": "orq-core::store a_slow_lock", "retry": "two"},
    ], "secret": "token-0123"});
    add_run(&s, "r-1", &project, &[("stage.started", json!({})), ("gate.flaky_tests", flaky), ("stage.completed", json!({"exit_code": 0}))]);

    let page = ask(&s, "/api/projects/alpha/runs/r-1");

    let events = page["events"].as_array().unwrap();
    let event = events.iter().find(|event| event["kind"] == "gate.flaky_tests").unwrap();
    assert_eq!(
        event["flaky_tests"],
        json!([
            {"name": "orq-cli::pipeline two_runs_in_one_second", "retry": 1},
            {"name": "orq-core::store a_slow_lock", "retry": null},
        ]),
        "{page}"
    );
    assert!(!page.to_string().contains("token-0123") && !page.to_string().contains("gate.log"), "{page}");
    assert!(events.iter().filter(|event| event["kind"] != "gate.flaky_tests").all(|event| event.get("flaky_tests").is_none()), "{page}");
}
