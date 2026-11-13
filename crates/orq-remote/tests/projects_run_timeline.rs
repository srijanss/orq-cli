//! `/api/projects/{id}/runs/{run}`: one run's persisted events as a timeline with a cursor, read-only, behind the same checks.

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
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: target.into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, stage, at)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: stage.map(String::from), attempt: 1, occurred_at: (*at).into(), payload: json!({"reason": "secret reason"}) };
        store.append_event(&event).unwrap();
    }
}

#[test]
fn a_run_gives_its_events_in_order_with_stage_attempt_and_time_and_never_the_payload() {
    let s = setup("timeline");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha), "develop", &[("run.started", None, "2026-11-01T10:00:00Z"), ("stage.started", Some("implement"), "2026-11-01T10:05:00Z"), ("stage.completed", Some("implement"), "2026-11-01T10:20:00Z")]);
    let (status, body) = get(&s, "/api/projects/alpha/runs/r1");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["run"], json!({"run_id": "r1", "feature": "f-r1", "target_branch": "develop", "state": "running", "stage": "implement", "started_at": "2026-11-01T10:00:00Z", "updated_at": "2026-11-01T10:20:00Z", "delivery": "review", "ending": null}), "{body}");
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "{body}");
    assert_eq!(events[1], json!({"event_id": "r1-1", "kind": "stage.started", "stage": "implement", "attempt": 1, "occurred_at": "2026-11-01T10:05:00Z"}), "{body}");
    assert_eq!(body["next_cursor"], Value::Null, "{body}");
    assert!(!body.to_string().contains("secret reason"), "{body}");
}

#[test]
fn the_cursor_resumes_after_an_event_and_an_unknown_cursor_is_not_found() {
    let s = setup("timeline-cursor");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha), "main", &[("run.started", None, "2026-11-01T10:00:00Z"), ("stage.started", Some("implement"), "2026-11-01T10:05:00Z"), ("stage.completed", Some("implement"), "2026-11-01T10:20:00Z")]);
    let (status, body) = get(&s, "/api/projects/alpha/runs/r1/after/r1-0");
    assert_eq!(status, 200, "{body}");
    let ids: Vec<_> = body["events"].as_array().unwrap().iter().map(|e| e["event_id"].clone()).collect();
    assert_eq!(ids, [json!("r1-1"), json!("r1-2")], "{body}");
    let (_, last) = get(&s, "/api/projects/alpha/runs/r1/after/r1-2");
    assert_eq!(last["events"], json!([]), "{last}");
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/after/nope").0, 404);
}

#[test]
fn a_long_timeline_is_paged_and_the_next_cursor_continues_it() {
    let s = setup("timeline-pages");
    let alpha = git_repo(&s.scan, "alpha");
    let events: Vec<(&str, Option<&str>, &str)> = (0..205).map(|_| ("stage.started", Some("implement"), "2026-11-01T10:00:00Z")).collect();
    add_run(&s, "r1", &project_id(&alpha), "main", &events);
    let (_, first) = get(&s, "/api/projects/alpha/runs/r1");
    assert_eq!(first["events"].as_array().unwrap().len(), 200, "a page is 200 events");
    let cursor = first["next_cursor"].as_str().unwrap().to_string();
    assert_eq!(cursor, "r1-199");
    let (_, second) = get(&s, &format!("/api/projects/alpha/runs/r1/after/{cursor}"));
    assert_eq!(second["events"].as_array().unwrap().len(), 5);
    assert_eq!(second["next_cursor"], Value::Null);
}

#[test]
fn another_projects_run_a_missing_run_and_a_bad_run_id_are_not_found_and_the_checks_apply() {
    let s = setup("timeline-scope");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, "r1", &project_id(&alpha), "main", &[("run.started", None, "2026-11-01T10:00:00Z")]);
    assert_eq!(get(&s, "/api/projects/beta/runs/r1").0, 404, "a run of another project");
    assert_eq!(get(&s, "/api/projects/alpha/runs/missing").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/..%2Fx").0, 404);
    assert_eq!(get(&s, "/api/projects/nope/runs/r1").0, 404);
    let _ = beta;
    assert_eq!(ask(&s, "GET", "/api/projects/alpha/runs/r1", None).0, 401);
    assert_eq!(ask(&s, "POST", "/api/projects/alpha/runs/r1", Some("Bearer tok")).0, 405);
}

#[test]
fn an_unreadable_log_gives_a_generic_error_that_names_nothing() {
    let s = setup("timeline-broken");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "bad", &project_id(&alpha), "main", &[("run.started", None, "2026-11-01T10:00:00Z")]);
    std::fs::write(s.roots.state.join("runs/bad/events.jsonl"), "{not json\n").unwrap();
    let (status, body) = get(&s, "/api/projects/alpha/runs/bad");
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(s.roots.state.to_str().unwrap()), "{body}");
}

#[test]
fn review_events_carry_their_run_wide_round_even_on_a_later_page() {
    let s = setup("timeline-rounds");
    let alpha = git_repo(&s.scan, "alpha");
    let mut events: Vec<(&str, Option<&str>, &str)> = vec![("stage.started", Some("review"), "2026-11-01T10:00:00Z"), ("stage.completed", Some("review"), "2026-11-01T10:01:00Z")];
    events.extend((0..198).map(|_| ("stage.log", Some("implement"), "2026-11-01T10:02:00Z")));
    events.extend([("stage.started", Some("review"), "2026-11-01T11:00:00Z"), ("stage.failed", Some("review"), "2026-11-01T11:01:00Z")]);
    add_run(&s, "r1", &project_id(&alpha), "main", &events);
    let (_, first) = get(&s, "/api/projects/alpha/runs/r1");
    let first = first["events"].as_array().unwrap().clone();
    assert_eq!(first[0]["review_round"], json!(1));
    assert_eq!(first[1]["review_round"], json!(1));
    assert!(first[2].get("review_round").is_none(), "only review events carry a round: {}", first[2]);
    let (_, second) = get(&s, "/api/projects/alpha/runs/r1/after/r1-199");
    let rounds: Vec<_> = second["events"].as_array().unwrap().iter().map(|e| e["review_round"].clone()).collect();
    assert_eq!(rounds, [json!(2), json!(2)], "a later page keeps counting from the whole run: {second}");
    assert_eq!(second["review_rounds"], json!(2), "{second}");
}
