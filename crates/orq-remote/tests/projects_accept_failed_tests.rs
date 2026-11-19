//! The run page's data names a failed Accept's log and the tests it failed, each with its output, and a failed
//! verify gate's failure carries the same list, all redacted.

mod web_deps;

use std::path::PathBuf;
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    project: String,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let project = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    Setup { scan, roots, project }
}

fn add_run(s: &Setup, run: &str, events: &[(&str, Option<&str>, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: s.project.clone(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, stage, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: s.project.clone(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: stage.map(String::from), attempt: 1, occurred_at: format!("2026-11-01T10:00:0{n}Z"), payload: payload.clone() };
        store.append_event(&event).unwrap();
    }
}

fn run_page(s: &Setup, run: &str) -> Value {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}");
    let request = Request { method: "GET", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

fn failed_tests() -> Value {
    json!([
        { "name": "store saves", "output": format!("sent {TOKEN}\n  left: 1\n right: 2") },
        { "name": "queue orders", "output": "the queue was empty" },
    ])
}

#[test]
fn a_failed_accept_names_its_log_file_and_lists_its_failed_tests_redacted() {
    let s = setup("accept-failed-tests");
    let log = s.roots.state.join("logs").join("r-1").join("accept-2.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "the whole output\n").unwrap();
    let failed = json!({ "reason": "2 tests failed: store saves, queue orders", "output": "tail", "log": log.to_str().unwrap(), "failed_tests": failed_tests(), "failed_tests_more": 3 });
    add_run(&s, "r-1", &[("run.started", None, json!({})), ("run.failed", None, json!({})), ("run.accept_started", None, json!({})), ("run.accept_failed", None, failed)]);

    let page = run_page(&s, "r-1");

    let failure = &page["run"]["accept_failure"];
    assert_eq!(failure["log"], "accept-2.log", "{failure}");
    assert_eq!(failure["failed_tests"][0]["name"], "store saves");
    assert_eq!(failure["failed_tests"][1]["output"], "the queue was empty");
    assert_eq!(failure["failed_tests_more"], 3);
    assert!(!failure.to_string().contains(TOKEN), "{failure}");
    assert!(failure["failed_tests"][0]["output"].as_str().unwrap().contains("right: 2"), "{failure}");
}

#[test]
fn an_accept_log_outside_the_runs_own_logs_is_not_named() {
    let s = setup("accept-failed-elsewhere");
    let failed = json!({ "reason": "x exited with status 1", "output": "tail", "log": "/etc/passwd" });
    add_run(&s, "r-1", &[("run.started", None, json!({})), ("run.failed", None, json!({})), ("run.accept_started", None, json!({})), ("run.accept_failed", None, failed)]);

    let failure = run_page(&s, "r-1")["run"]["accept_failure"].clone();

    assert!(failure["log"].is_null(), "{failure}");
    assert_eq!(failure["failed_tests"], json!([]), "{failure}");
}

#[test]
fn a_failed_verify_gate_lists_its_failed_tests_redacted() {
    let s = setup("gate-failed-tests");
    let failed = json!({ "reason": "gate step 1 (cargo) exited with status 100", "exit_code": 100, "failed_tests": failed_tests(), "failed_tests_more": 0 });
    add_run(&s, "r-1", &[("run.started", None, json!({})), ("stage.started", Some("verify"), json!({})), ("stage.failed", Some("verify"), failed)]);

    let page = run_page(&s, "r-1");

    let event = page["events"].as_array().unwrap().iter().find(|event| event["kind"] == "stage.failed").unwrap().clone();
    let failure = &event["failure"];
    assert_eq!(failure["failed_tests"][1]["name"], "queue orders", "{failure}");
    assert_eq!(failure["failed_tests_more"], 0);
    assert!(!failure.to_string().contains(TOKEN), "{failure}");
}
