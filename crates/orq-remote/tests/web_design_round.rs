//! A frontend run's design review step shows its round on the run page: the run's events carry
//! `design_round` ("design_review round 2 of 4") and the Timeline names the step with it.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

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

/// Events as (kind, stage, attempt, payload).
fn add_run(s: &Setup, run: &str, project: &str, events: &[(&str, &str, u32, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, stage, attempt, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: Some((*stage).into()), attempt: *attempt, occurred_at: "2026-11-01T10:00:00Z".into(), payload: payload.clone() };
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
fn the_design_review_events_name_their_round_and_no_other_event_has_one() {
    let s = setup("design-round-shown");
    let alpha = git_repo(&s.scan, "alpha");
    let project = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    add_run(&s, "r-1", &project, &[
        ("stage.started", "implement_and_review", 1, json!({})),
        ("stage.completed", "implement_and_review", 1, json!({})),
        ("stage.started", "design_review", 1, json!({"round": 1, "of": 4})),
        ("design_review", "design_review", 1, json!({"round": 1, "of": 4, "verdict": "changes_required", "head": "abc", "report": "/secret/report.md"})),
        ("design_correction", "design_review", 1, json!({"batch": 1, "of": 3, "round": 1, "reviews_of": 4, "from": "abc"})),
        ("stage.started", "design_review", 2, json!({"round": 2, "of": 4})),
    ]);

    let page = ask(&s, "/api/projects/alpha/runs/r-1");

    let events = page["events"].as_array().unwrap();
    let rounds: Vec<&Value> = events.iter().map(|event| &event["design_round"]).collect();
    assert_eq!(
        rounds,
        [&Value::Null, &Value::Null, &json!("design_review round 1 of 4"), &json!("design_review round 1 of 4"), &json!("design_review round 1 of 4"), &json!("design_review round 2 of 4")],
        "{page}"
    );
    assert!(!page.to_string().contains("/secret/report.md"), "the payload stays out: {page}");
}

#[test]
fn the_timeline_names_a_design_review_step_with_its_round() {
    if !web_deps::ready() {
        return;
    }
    let event = |id: &str, kind: &str, stage: &str, attempt: u32, round: Option<&str>| {
        let mut event = json!({"event_id": id, "kind": kind, "stage": stage, "attempt": attempt, "occurred_at": "2026-11-16T15:05:51Z"});
        if let Some(round) = round {
            event["design_round"] = json!(round);
        }
        event
    };
    let events = json!([
        event("e0", "stage.started", "implement_and_review", 1, None),
        event("e1", "stage.completed", "implement_and_review", 1, None),
        event("e2", "stage.started", "design_review", 1, Some("design_review round 1 of 4")),
        event("e3", "design_review", "design_review", 1, Some("design_review round 1 of 4")),
        event("e4", "stage.started", "design_review", 2, Some("design_review round 2 of 4")),
    ]);
    let run = json!({"run_id": "run-1", "feature": "page", "target_branch": "main", "state": "running", "stage": "design_review", "started_at": "2026-11-16T15:05:49Z", "updated_at": "2026-11-16T15:35:11Z"});
    let page = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null}});
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 1, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": "/projects/alpha/orchestrations/run-1", "responses": [page, projects], "steps": [{"submit": TOKEN}], "selectors": [".run-step"]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let texts: Vec<String> = out["frames"][1]["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect();
    assert!(texts.iter().any(|t| t == "design_review round 1 of 4"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "design_review round 2 of 4"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.starts_with("design_review · attempt")), "{texts:?}");
}
