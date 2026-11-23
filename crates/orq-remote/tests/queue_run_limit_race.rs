//! A detached run the queue launched may not have written its record when the queue starts the next
//! entry, and its runner checks the run limits again on its own. A run the queue started earlier in a
//! round holds its slot for the rest of that round, record or not; a start its runner then refuses for
//! a run limit is recorded so, and the queue puts the entry back to wait instead of failing it. A run
//! with no record and no such refusal still fails its entry.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::project_settings::ProjectSettings;
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, ResumeRequest, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Starts detached runs: each launch returns its run id at once, and the run's record is written
/// only when `records` is on, as a runner that got going would.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<(String, String)>>,
    records: Mutex<bool>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let mut started = self.started.lock().unwrap();
        let run = format!("run-{}-{feature}", started.len() + 1);
        started.push((feature.to_string(), run.clone()));
        if *self.records.lock().unwrap() {
            seed_run(&self.state, repo, &run, feature);
        }
        Ok(run)
    }

    fn resume_with(&self, _repo: &Path, run: &str, _request: &ResumeRequest) -> Result<String, LaunchError> {
        Err(LaunchError::Failed(format!("{run} is not resumed here")))
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
}

/// A machine whose config adds `runs`, with no project yet; its launches write no record.
fn setup(label: &str, runs: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), format!("remote_actions = true\n{runs}")).unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default(), records: Mutex::new(false) });
    Setup { scan: base.join("code"), roots, launcher }
}

/// A repository `name` whose ledger holds the independent pending `features`, with at most
/// `max_active_runs` and stop on failure on.
fn git_repo(s: &Setup, name: &str, features: &[&str], max_active_runs: Option<u8>) -> PathBuf {
    let path = s.scan.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let path = std::fs::canonicalize(path).unwrap();
    let ledger: Vec<Value> = features.iter().map(|name| json!({"featureName": name, "dependsOn": [], "status": "pending"})).collect();
    std::fs::write(path.join(".tdd-features.json"), Value::Array(ledger).to_string()).unwrap();
    let settings = ProjectSettings { max_active_runs, queue_stop_on_failure: true, ..ProjectSettings::default() };
    Store::open(&s.roots.database()).unwrap().save_project_settings(&path, 0, &settings).unwrap();
    path
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

fn queue_add(s: &Setup, project: &str, features: &[&str]) {
    let (status, answer) = send(s, "POST", &format!("/api/projects/{project}/queue"), json!({"features": features}).to_string().as_bytes());
    assert_eq!(status, 200, "{answer}");
}

/// The queue as the page reads it.
fn queue(s: &Setup, project: &str) -> Value {
    let (status, body) = send(s, "GET", &format!("/api/projects/{project}/queue"), b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// The page's entry for `feature`.
fn entry(s: &Setup, project: &str, feature: &str) -> Value {
    let q = queue(s, project);
    q["entries"].as_array().unwrap().iter().find(|entry| entry["feature"] == feature).cloned().unwrap_or_else(|| panic!("{feature} is not queued: {q}"))
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

/// The features launched so far, in order.
fn started(s: &Setup) -> Vec<String> {
    s.launcher.started.lock().unwrap().iter().map(|(feature, _)| feature.clone()).collect()
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
            occurred_at: format!("2026-11-24T07:35:{n:02}Z"),
            payload,
        })
        .unwrap();
}

/// A run of `feature` in the project at `repo`, started and still running.
fn seed_run(state: &Path, repo: &Path, run: &str, feature: &str) {
    let store = RunStore::new(state);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

/// The run the queue launched for `feature`.
fn run_of(s: &Setup, feature: &str) -> String {
    s.launcher.started.lock().unwrap().iter().rev().find(|(started, _)| started == feature).map(|(_, run)| run.clone()).unwrap()
}

/// The runner of `run` refuses it, as `orq run` does, for the `limit` (`project` or `machine`) it found reached.
fn refuse(s: &Setup, run: &str, limit: orq_orchestrator::RunLimit) {
    let refusal = orq_orchestrator::LimitRefusal { reason: "alpha already has 1 active run (run-other, OTHER): wait for it to finish, or raise the limit in project settings".into(), limit };
    orq_orchestrator::record_limit_refusal(&RunStore::new(&s.roots.state), run, &refusal).unwrap();
}

/// The entry for `feature` waits again for a slot: no run, not failed, the queue not paused.
fn waits_for_a_slot(s: &Setup, project: &str, feature: &str) {
    let q = queue(s, project);
    assert_eq!(q["paused"], Value::Null, "a start its runner refused for a run limit waits: {q}");
    let waiting = entry(s, project, feature);
    assert_eq!((&waiting["run"], &waiting["state"], &waiting["reason"]), (&Value::Null, &json!("waiting"), &json!("run limit reached")), "{q}");
}

#[test]
fn a_start_its_runner_refuses_on_the_project_limit_waits_for_a_slot() {
    let s = setup("qrlr-refused-project", "");
    let alpha = git_repo(&s, "alpha", &["A-01"], Some(1));
    queue_add(&s, "alpha", &["A-01"]);
    round(&s);
    // Another run took the slot before the runner of A-01 recorded its run, so the runner refused it.
    seed_run(&s.roots.state, &alpha, "run-other", "OTHER");
    refuse(&s, &run_of(&s, "A-01"), orq_orchestrator::RunLimit::Project);

    round(&s);
    waits_for_a_slot(&s, "alpha", "A-01");
    assert_eq!(started(&s), ["A-01"], "no start while the slot is taken");

    event(&RunStore::new(&s.roots.state), "run-other", "run.completed", json!({"ending": "merged"}));
    *s.launcher.records.lock().unwrap() = true;
    round(&s);
    assert_eq!(started(&s), ["A-01", "A-01"], "started again once the slot freed");
    let started_again = entry(&s, "alpha", "A-01");
    assert_eq!((&started_again["run"], &started_again["state"], &started_again["reason"]), (&json!(run_of(&s, "A-01")), &json!("running"), &Value::Null));
}

#[test]
fn a_start_its_runner_refuses_on_the_machine_limit_waits_for_a_slot() {
    let s = setup("qrlr-refused-machine", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &["A-01"], None);
    let beta = git_repo(&s, "beta", &[], None);
    queue_add(&s, "alpha", &["A-01"]);
    round(&s);
    // A run of another project took the machine's slot before the runner of A-01 recorded its run.
    seed_run(&s.roots.state, &beta, "run-other", "OTHER");
    refuse(&s, &run_of(&s, "A-01"), orq_orchestrator::RunLimit::Machine);

    round(&s);
    waits_for_a_slot(&s, "alpha", "A-01");
    assert_eq!(started(&s), ["A-01"], "no start while the machine's slot is taken");

    event(&RunStore::new(&s.roots.state), "run-other", "run.completed", json!({"ending": "merged"}));
    *s.launcher.records.lock().unwrap() = true;
    round(&s);
    assert_eq!(started(&s), ["A-01", "A-01"], "started again once the slot freed");
    let started_again = entry(&s, "alpha", "A-01");
    assert_eq!((&started_again["run"], &started_again["state"]), (&json!(run_of(&s, "A-01")), &json!("running")));
}

#[test]
fn a_launched_run_with_no_record_and_no_refusal_still_fails_its_entry() {
    let s = setup("qrlr-missing", "");
    git_repo(&s, "alpha", &["A-01"], Some(1));
    queue_add(&s, "alpha", &["A-01"]);
    round(&s);

    round(&s);
    let q = queue(&s, "alpha");
    assert!(q["paused"].as_str().is_some_and(|why| why.contains("cannot be read")), "stop on failure pauses the queue: {q}");
    let failed = entry(&s, "alpha", "A-01");
    assert_eq!(failed["state"], json!("failed"), "{q}");
    assert_eq!(started(&s), ["A-01"]);
}

#[test]
fn a_run_started_earlier_in_the_round_holds_the_project_slot_before_its_record_exists() {
    let s = setup("qrlr-round-project", "");
    git_repo(&s, "alpha", &["A-01", "A-02"], Some(1));
    queue_add(&s, "alpha", &["A-01", "A-02"]);

    round(&s);
    assert_eq!(started(&s), ["A-01"], "the second start waits for the slot the first holds");
    let q = queue(&s, "alpha");
    assert_eq!(q["paused"], Value::Null, "{q}");
    let second = entry(&s, "alpha", "A-02");
    assert_eq!((&second["run"], &second["state"]), (&Value::Null, &json!("next")), "{q}");
}

#[test]
fn a_run_started_earlier_in_the_round_holds_the_machine_slot_before_its_record_exists() {
    let s = setup("qrlr-round-machine", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &["A-01"], None);
    git_repo(&s, "beta", &["B-01"], None);
    queue_add(&s, "alpha", &["A-01"]);
    queue_add(&s, "beta", &["B-01"]);

    round(&s);
    assert_eq!(started(&s).len(), 1, "the machine's one slot is held by the first start: {:?}", started(&s));
    for project in ["alpha", "beta"] {
        assert_eq!(queue(&s, project)["paused"], Value::Null, "{project}");
    }
}
