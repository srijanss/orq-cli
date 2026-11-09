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

fn seeded(label: &str) -> Setup {
    let s = setup(label);
    let alpha = git_repo(&s.scan, "alpha");
    let a = project_id(&alpha);
    add_run(&s, "r1", &a, "main", &[("run.started", None, "2026-11-01T10:00:00Z"), ("run.failed", None, "2026-11-01T11:00:00Z")]);
    add_run(&s, "r2", &a, "main", &[("run.started", None, "2026-11-02T10:00:00Z")]);
    add_run(&s, "r3", &a, "main", &[("run.started", None, "2026-11-03T10:00:00Z"), ("run.failed", None, "2026-11-03T11:00:00Z")]);
    add_run(&s, "r4", &a, "main", &[("run.started", None, "2026-11-04T10:00:00Z")]);
    s
}

fn ids(body: &Value) -> Vec<String> {
    body["runs"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_page_of_runs_gives_the_next_cursor_and_the_cursor_continues_where_it_stopped() {
    let s = seeded("runs-paging");
    let (status, first) = get(&s, "/api/projects/alpha/runs?limit=3");
    assert_eq!(status, 200, "{first}");
    assert_eq!(ids(&first), ["r4", "r3", "r2"], "{first}");
    let cursor = first["next_cursor"].as_str().expect("more runs remain").to_string();
    let (_, second) = get(&s, &format!("/api/projects/alpha/runs?limit=3&cursor={cursor}"));
    assert_eq!(ids(&second), ["r1"], "{second}");
    assert_eq!(second["next_cursor"], Value::Null, "{second}");
}

#[test]
fn the_running_count_covers_the_whole_set_not_the_page_or_the_filter() {
    let s = seeded("runs-count");
    let (_, page) = get(&s, "/api/projects/alpha/runs?limit=1&state=failed");
    assert_eq!(ids(&page), ["r3"], "{page}");
    assert_eq!(page["running"], 2, "{page}");
    assert_eq!(page["total"], 2, "{page}");
}

#[test]
fn runs_sort_oldest_first_on_request_and_an_unknown_sort_or_limit_is_refused() {
    let s = seeded("runs-sort");
    let (_, body) = get(&s, "/api/projects/alpha/runs?sort=oldest");
    assert_eq!(ids(&body), ["r1", "r2", "r3", "r4"], "{body}");
    assert_eq!(get(&s, "/api/projects/alpha/runs?sort=sideways").0, 400);
    assert_eq!(get(&s, "/api/projects/alpha/runs?limit=0").0, 400);
    assert_eq!(get(&s, "/api/projects/alpha/runs?limit=abc").0, 400);
}

#[test]
fn a_cursor_naming_no_run_is_not_found() {
    let s = seeded("runs-cursor");
    assert_eq!(get(&s, "/api/projects/alpha/runs?cursor=nope").0, 404);
}
