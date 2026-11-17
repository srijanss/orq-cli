//! A failure reported unrelated is run again once. The Timeline says so under the step: passed on a
//! re-run, or failed again, with what the fix session reported (redacted, never the whole payload).

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SUMMARY: &str = "billing clock test is flaky";

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

/// A run of `repo` with `events` as (kind, stage, attempt, payload).
fn add_run(s: &Setup, run: &str, repo: &Path, events: &[(&str, &str, u32, Value)]) {
    let project = orq_git::project_id(&orq_git::resolve_project(repo).unwrap());
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.clone(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, stage, attempt, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.clone(), feature_id: "f".into(), kind: (*kind).into(), stage: Some((*stage).into()), attempt: *attempt, occurred_at: format!("2026-11-17T10:00:{n:02}Z"), payload: payload.clone() };
        store.append_event(&event).unwrap();
    }
}

fn timeline(s: &Setup, run: &str) -> Value {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}");
    let request = Request { method: "GET", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let response = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    assert_eq!(response.status, 200, "{}", response.body);
    response.body
}

fn shown(page: &Value, kind: &str) -> Value {
    page["events"].as_array().unwrap().iter().find(|event| event["kind"] == kind).unwrap_or_else(|| panic!("no {kind}: {page}")).clone()
}

#[test]
fn a_re_run_event_shows_its_summary_and_whether_it_passed_and_nothing_else_of_its_payload() {
    let s = setup("flaky-shown");
    let alpha = git_repo(&s.scan, "alpha");
    let secret = "token ghp_0123456789abcdef0123456789abcdef0123";
    add_run(
        &s,
        "r-1",
        &alpha,
        &[
            ("stage.started", "verify", 2, json!({})),
            ("gate.flaky", "verify", 2, json!({"summary": SUMMARY, "step": 1})),
            ("stage.completed", "verify", 2, json!({})),
            ("ci.rerun_failed", "ci", 1, json!({"summary": format!("deploy broke: {secret}"), "failing": ["deploy"], "rerun": "the re-run failed too"})),
        ],
    );

    let page = timeline(&s, "r-1");
    assert_eq!(shown(&page, "gate.flaky")["flaky"], json!({"summary": SUMMARY, "passed": true}), "{page}");
    let failed = shown(&page, "ci.rerun_failed")["flaky"].clone();
    assert_eq!(failed["passed"], json!(false), "{page}");
    assert!(!failed["summary"].as_str().unwrap().contains("ghp_0123456789"), "the summary is redacted: {page}");
    assert!(shown(&page, "stage.started").get("flaky").is_none(), "{page}");
}

/// The run page in the browser harness, or `None` when the web dependencies are not installed.
fn open_run(events: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let run = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "merged", "stage": "verify", "started_at": "2026-11-17T10:00:00Z", "updated_at": "2026-11-17T10:00:09Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": [page, projects], "steps": [{"submit": TOKEN}], "selectors": [".run-flaky-note"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    Some(out["frames"][1].clone())
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn event(id: &str, kind: &str, stage: &str, attempt: u32, at: &str, flaky: Option<Value>) -> Value {
    let mut event = json!({"event_id": id, "kind": kind, "stage": stage, "attempt": attempt, "occurred_at": at});
    if let Some(flaky) = flaky {
        event["flaky"] = flaky;
    }
    event
}

#[test]
fn the_timeline_notes_a_step_that_passed_on_a_re_run_and_one_whose_re_run_failed_too() {
    let events = json!([
        event("e0", "stage.started", "verify", 2, "2026-11-17T10:00:00Z", None),
        event("e1", "gate.flaky", "verify", 2, "2026-11-17T10:00:05Z", Some(json!({"summary": SUMMARY, "passed": true}))),
        event("e2", "stage.completed", "verify", 2, "2026-11-17T10:00:06Z", None),
        event("e3", "stage.started", "ci", 1, "2026-11-17T10:00:07Z", None),
        event("e4", "ci.rerun_failed", "ci", 1, "2026-11-17T10:00:09Z", Some(json!({"summary": "deploy check is flaky", "passed": false}))),
    ]);
    let Some(frame) = open_run(events) else { return };

    assert_eq!(frame["settled"]["matches"][".run-flaky-note"], json!(2), "{frame}");
    let texts = texts(&frame);
    let says = |text: &str| texts.iter().any(|t| t.contains(text));
    assert!(says(&format!("passed on a re-run; the first failure was reported unrelated: {SUMMARY}")), "{texts:?}");
    assert!(says("the re-run failed too; the failure was reported unrelated: deploy check is flaky"), "{texts:?}");
}

#[test]
fn a_step_with_no_re_run_has_no_flaky_note() {
    let events = json!([
        event("e0", "stage.started", "verify", 1, "2026-11-17T10:00:00Z", None),
        event("e1", "stage.completed", "verify", 1, "2026-11-17T10:00:06Z", None),
    ]);
    let Some(frame) = open_run(events) else { return };
    assert_eq!(frame["settled"]["matches"][".run-flaky-note"], json!(0), "{frame}");
}
