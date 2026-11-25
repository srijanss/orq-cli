//! A queued entry whose run is paused says so: a run paused by a person, one whose pause was asked for and
//! one that stopped for the design files it needs show as "paused" with why, on the page and in
//! `orq queue list`, while a running run stays "running". The phase is read through the runs a run was
//! resumed as. Each round is driven here with a fake launcher, and the run's phase is what its events say.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::project_settings::ProjectSettings;
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, RunLauncher, StoreProjects};
use serde_json::{json, Value};

const DESIGN_FILES: &str = "Waiting for design files: add the files the run page lists, then resume";
const PAUSED: &str = "Paused: resume it from the run page";
const PAUSING: &str = "Pausing";

/// Starts a run the way `orq run` records one.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<String>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature);
        Ok(run)
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
    repo: PathBuf,
}

/// A project `alpha` with two approved features queued, COM-01 and COM-02 (which depends on COM-01),
/// at most one run active.
fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default() });
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let repo = std::fs::canonicalize(path).unwrap();
    let ledger = json!([{"featureName": "COM-01", "dependsOn": [], "status": "pending"}, {"featureName": "COM-02", "dependsOn": ["COM-01"], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let settings = ProjectSettings { max_active_runs: Some(1), ..ProjectSettings::default() };
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup { scan, roots, launcher, repo };
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue", json!({"features": ["COM-01", "COM-02"]}).to_string().as_bytes());
    assert_eq!(status, 200, "{body}");
    s
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone())
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider(s));
    (r.status, r.body)
}

/// The queue as the page reads it.
fn queue(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// The queue as `orq queue list` reads it.
fn listed(s: &Setup) -> Value {
    let git = orq_git::resolve_project(&s.repo).unwrap();
    let store = Store::open(&s.roots.database()).unwrap();
    orq_remote::projects::project_queue_json(&store, &RunStore::new(&s.roots.state), &git, &s.repo).unwrap()
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

fn started(s: &Setup) -> Vec<String> {
    s.launcher.started.lock().unwrap().clone()
}

fn event(store: &RunStore, run: &str, kind: &str, payload: Value) {
    let record = store.load(run).unwrap();
    let n = store.events(run).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: format!("2026-11-24T10:00:{n:02}Z"),
            payload,
        })
        .unwrap();
}

/// A run of `feature` in the project at `repo`, started and still running.
fn seed_run(state: &Path, repo: &Path, run: &str, feature: &str) {
    let store = RunStore::new(state);
    let head = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap();
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: String::from_utf8_lossy(&head.stdout).trim().to_string(),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

fn runs(s: &Setup) -> RunStore {
    RunStore::new(&s.roots.state)
}

/// The first entry's state and reason.
fn first(queue: &Value) -> (Value, Value) {
    (queue["entries"][0]["state"].clone(), queue["entries"][0]["reason"].clone())
}

#[test]
fn an_entry_whose_run_waits_for_design_files_is_paused_and_its_dependent_still_waits_for_them() {
    let s = setup("paused-design-files");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    event(&runs(&s), "run-1-COM-01", "run.completed", json!({"ending": "design_files_needed"}));
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "a pause is not a failure: the queue still holds");

    let shown = queue(&s);
    assert_eq!(first(&shown), (json!("paused"), json!(DESIGN_FILES)), "{shown}");
    assert_eq!(shown["entries"][0]["run"], "run-1-COM-01", "{shown}");
    assert_eq!(shown["entries"][1]["state"], "waiting", "{shown}");
    assert_eq!(shown["entries"][1]["reason"], "waiting for COM-01 (waiting for design files)", "{shown}");
    assert_eq!(listed(&s), shown, "the terminal and the page show the same queue");
}

#[test]
fn a_paused_and_a_pause_requested_run_are_paused_and_a_running_run_stays_running() {
    let s = setup("paused-running");
    round(&s);
    assert_eq!(first(&queue(&s)), (json!("running"), Value::Null));

    event(&runs(&s), "run-1-COM-01", "run.pause_requested", json!({}));
    assert_eq!(first(&queue(&s)), (json!("paused"), json!(PAUSING)));
    assert_eq!(first(&listed(&s)), (json!("paused"), json!(PAUSING)));

    event(&runs(&s), "run-1-COM-01", "run.paused", json!({}));
    assert_eq!(first(&queue(&s)), (json!("paused"), json!(PAUSED)));
    assert_eq!(first(&listed(&s)), (json!("paused"), json!(PAUSED)));

    event(&runs(&s), "run-1-COM-01", "run.resumed", json!({}));
    assert_eq!(first(&queue(&s)), (json!("running"), Value::Null), "resumed in place, it runs again");
}

#[test]
fn an_entry_whose_run_was_resumed_as_another_follows_it_to_its_phase() {
    let s = setup("paused-resumed-as");
    round(&s);
    seed_run(&s.roots.state, &s.repo, "run-9-COM-01", "COM-01");
    event(&runs(&s), "run-9-COM-01", "run.completed", json!({"ending": "design_files_needed"}));
    event(&runs(&s), "run-1-COM-01", "run.resumed", json!({"resumed_as": "run-9-COM-01"}));
    assert_eq!(first(&queue(&s)), (json!("paused"), json!(DESIGN_FILES)), "{}", queue(&s));
    assert_eq!(first(&listed(&s)), (json!("paused"), json!(DESIGN_FILES)));
}

mod web {
    use super::*;

    const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
    const SELECTORS: [&str; 5] = ["[data-queue-state=\"paused\"]", "[data-queue-state=\"paused\"] .icon", "[data-queue-marker=\"running\"]", "[data-queue-state=\"running\"]", "[data-open-run]"];

    fn run(url: &str, responses: Vec<Value>) -> Option<Value> {
        if !web_deps::ready() {
            return None;
        }
        let dir = env!("CARGO_MANIFEST_DIR");
        let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
        let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Some(serde_json::from_slice(&output.stdout).unwrap())
    }

    fn paused_queue() -> Value {
        let entry = json!({"feature": "landing-page", "kind": "start", "label": "Start landing-page", "state": "paused", "run": "run-1795488221-41428-1", "reason": DESIGN_FILES, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null, "mr_url": null});
        json!({"entries": [entry], "count": 1, "paused": null, "waiting_for_review": null})
    }

    fn reply(queue: Value) -> Value {
        json!({"status": 200, "json": {"queue": queue, "remote_actions": true}})
    }

    fn project(id: &str) -> Value {
        json!({"id": id, "name": id, "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": null})
    }

    fn projects(list: Vec<Value>) -> Value {
        json!({"status": 200, "json": {"projects": list}})
    }

    fn last(out: &Value) -> &Value {
        out["frames"].as_array().unwrap().last().unwrap()
    }

    fn count(frame: &Value, selector: &str) -> u64 {
        frame["settled"]["matches"][selector].as_u64().unwrap()
    }

    fn shows(frame: &Value, wanted: &str) -> bool {
        frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).any(|t| t.contains(wanted))
    }

    fn assert_paused_shown(out: &Value) {
        let frame = last(out);
        assert_eq!(count(frame, "[data-queue-state=\"paused\"]"), 1, "{out}");
        assert_eq!(count(frame, "[data-queue-state=\"paused\"] .icon"), 1, "the badge carries the pause icon: {out}");
        assert_eq!((count(frame, "[data-queue-marker=\"running\"]"), count(frame, "[data-queue-state=\"running\"]")), (0, 0), "not the running indicator: {out}");
        assert_eq!(count(frame, "[data-open-run]"), 1, "the paused entry links its run: {out}");
        assert!(shows(frame, "Paused"), "{out}");
        assert!(shows(frame, DESIGN_FILES), "{out}");
    }

    #[test]
    fn the_queue_tab_and_the_all_projects_queue_show_a_paused_entry_with_the_pause_icon_and_its_reason() {
        let Some(out) = run("/projects/alpha/queue", vec![reply(paused_queue()), projects(vec![project("alpha")])]) else { return };
        assert_paused_shown(&out);

        let empty = json!({"status": 200, "json": {"queue": {"entries": [], "count": 0, "paused": null, "waiting_for_review": null}, "remote_actions": true}});
        let Some(out) = run("/queue", vec![projects(vec![project("alpha"), project("beta")]), reply(paused_queue()), empty]) else { return };
        assert_paused_shown(&out);
    }
}
