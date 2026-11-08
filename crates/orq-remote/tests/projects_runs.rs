//! `/api/projects/{id}/runs`: that project's orchestration runs, newest first, read-only, behind the same checks.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

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

fn ask(setup: &Setup, method: &str, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let provider = StoreProjects::new(setup.roots.clone(), setup.scan.clone());
    ask_with(&provider, setup, method, path, authorization)
}

fn ask_with(provider: &StoreProjects, setup: &Setup, method: &str, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with(&setup.roots, &config, "server-project", "server", &request, provider);
    (r.status, r.body)
}

fn get(setup: &Setup, path: &str) -> (u16, Value) {
    ask(setup, "GET", path, Some("Bearer tok"))
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

fn add_run(s: &Setup, run: &str, project: &str, target: &str, events: &[(&str, Option<&str>, &str)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: target.into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default() })
        .unwrap();
    for (n, (kind, stage, at)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: stage.map(String::from), attempt: 1, occurred_at: (*at).into(), payload: json!({"reason": "secret reason"}) };
        store.append_event(&event).unwrap();
    }
}

#[test]
fn a_project_lists_only_its_own_runs_newest_first_with_state_stage_feature_branch_and_times() {
    let s = setup("runs-list");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    let (a, b) = (project_id(&alpha), project_id(&beta));
    add_run(&s, "old", &a, "main", &[("run.started", None, "2026-11-01T10:00:00Z"), ("run.completed", None, "2026-11-01T11:00:00Z")]);
    add_run(&s, "new", &a, "develop", &[("run.started", None, "2026-11-02T10:00:00Z"), ("stage.started", Some("implement"), "2026-11-02T10:30:00Z")]);
    add_run(&s, "other", &b, "main", &[("run.started", None, "2026-11-03T10:00:00Z")]);

    let (status, body) = get(&s, "/api/projects/alpha/runs");
    assert_eq!(status, 200, "{body}");
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{body}");
    assert_eq!(runs[0], json!({"run_id": "new", "feature": "f-new", "target_branch": "develop", "state": "running", "stage": "implement", "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z"}), "{body}");
    assert_eq!(runs[1]["run_id"], "old", "{body}");
    assert_eq!(runs[1]["stage"], Value::Null, "{body}");
    assert!(!body.to_string().contains("secret reason"), "{body}");
    let (_, other) = get(&s, "/api/projects/beta/runs");
    assert_eq!(other["runs"].as_array().unwrap().len(), 1, "{other}");
}

#[test]
fn a_project_without_runs_is_empty_and_an_unknown_project_is_not_found() {
    let s = setup("runs-none");
    git_repo(&s.scan, "alpha");
    let (status, body) = get(&s, "/api/projects/alpha/runs");
    assert_eq!((status, &body["runs"]), (200, &json!([])), "{body}");
    assert_eq!(get(&s, "/api/projects/nope/runs").0, 404);
}

#[test]
fn the_runs_need_the_token_and_only_read() {
    let s = setup("runs-auth");
    git_repo(&s.scan, "alpha");
    assert_eq!(ask(&s, "GET", "/api/projects/alpha/runs", None).0, 401);
    assert_eq!(ask(&s, "GET", "/api/projects/alpha/runs", Some("Bearer wrong")).0, 401);
    assert_eq!(ask(&s, "POST", "/api/projects/alpha/runs", Some("Bearer tok")).0, 405);
    assert_eq!(ask(&s, "HEAD", "/api/projects/alpha/runs", Some("Bearer tok")).0, 200);
}

#[test]
fn an_unreadable_run_gives_a_generic_error_that_names_nothing() {
    let s = setup("runs-broken");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "bad", &project_id(&alpha), "main", &[("run.started", None, "2026-11-01T10:00:00Z")]);
    let events = std::fs::read_dir(&s.roots.state).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>();
    let mut broke = false;
    let mut stack = events;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(std::fs::read_dir(&path).unwrap().flatten().map(|e| e.path()));
        } else if path.to_string_lossy().contains("events") {
            std::fs::write(&path, "{not json\n").unwrap();
            broke = true;
        }
    }
    assert!(broke, "found the event log to damage");
    let (status, body) = get(&s, "/api/projects/alpha/runs");
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(s.roots.state.to_str().unwrap()) && !body.to_string().contains("bad"), "{body}");
}

#[test]
fn a_log_that_has_not_changed_is_not_read_again_and_one_that_grew_is() {
    let s = setup("runs-cache");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha), "main", &[("run.started", None, "2026-11-01T10:00:00Z")]);
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let list = |p: &StoreProjects| ask_with(p, &s, "GET", "/api/projects/alpha/runs", Some("Bearer tok"));
    let (status, first) = list(&provider);
    assert_eq!((status, first["runs"][0]["state"].clone()), (200, json!("running")), "{first}");

    // Damage the log without changing its size or time: only a re-read would notice.
    let log = s.roots.state.join("runs/r1/events.jsonl");
    let (len, modified) = { let m = std::fs::metadata(&log).unwrap(); (m.len() as usize, m.modified().unwrap()) };
    std::fs::write(&log, "x".repeat(len)).unwrap();
    std::fs::File::options().write(true).open(&log).unwrap().set_modified(modified).unwrap();
    let (status, again) = list(&provider);
    assert_eq!((status, &again), (200, &first), "an unchanged log is served from what was learned");

    // A longer log is read afresh (here it is unreadable, so the damage now shows).
    std::fs::write(&log, "x".repeat(len + 5)).unwrap();
    assert_eq!(list(&provider).0, 500);
    // A new provider (a restart) never trusts anything it was not told.
    std::fs::write(&log, "x".repeat(len)).unwrap();
    assert_eq!(list(&StoreProjects::new(s.roots.clone(), s.scan.clone())).0, 500);
}
