//! A queued start or action that the launch itself finds over the project's or the machine's run
//! limit, because a run became active between the round's check and the launch, is waited out like a
//! refused one: the queue is not paused and the entry starts once a slot frees. Any other launch
//! error still pauses the queue.

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

/// What a launch does once, before it fails: another start taking the last slot meanwhile.
type Meanwhile = Box<dyn FnOnce() + Send>;

/// Starts and resumes runs the way `orq run` records them; the next launch fails with `fail`, after
/// doing what `meanwhile` holds.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<String>>,
    resumed: Mutex<Vec<String>>,
    fail: Mutex<Option<LaunchError>>,
    meanwhile: Mutex<Option<Meanwhile>>,
}

impl Launcher {
    /// The failure this launch ends with, after what happens meanwhile.
    fn failure(&self) -> Option<LaunchError> {
        let failure = self.fail.lock().unwrap().take()?;
        if let Some(change) = self.meanwhile.lock().unwrap().take() {
            change();
        }
        Some(failure)
    }
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        if let Some(failure) = self.failure() {
            return Err(failure);
        }
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature);
        Ok(run)
    }

    fn resume_with(&self, repo: &Path, run: &str, _request: &ResumeRequest) -> Result<String, LaunchError> {
        if let Some(failure) = self.failure() {
            return Err(failure);
        }
        let mut resumed = self.resumed.lock().unwrap();
        resumed.push(run.to_string());
        let store = RunStore::new(&self.state);
        let feature = store.load(run).unwrap().feature_id;
        let next = format!("{run}-r{}", resumed.len());
        seed_run(&self.state, repo, &next, &feature);
        event(&store, run, "run.resumed", json!({"resumed_as": next}));
        Ok(next)
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
}

/// A machine whose config adds `runs`, with no project yet.
fn setup(label: &str, runs: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), format!("remote_actions = true\n{runs}")).unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default(), resumed: Mutex::default(), fail: Mutex::default(), meanwhile: Mutex::default() });
    Setup { scan: base.join("code"), roots, launcher }
}

/// A repository `name` whose ledger holds the pending `features`, with at most `max_active_runs`.
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
    let settings = ProjectSettings { max_active_runs, ..ProjectSettings::default() };
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

/// Adds to project `project`'s queue as the page does.
fn queue_add(s: &Setup, project: &str, body: Value) {
    let (status, answer) = send(s, "POST", &format!("/api/projects/{project}/queue"), body.to_string().as_bytes());
    assert_eq!(status, 200, "{answer}");
}

/// The queue as the page reads it.
fn queue(s: &Setup, project: &str) -> Value {
    let (status, body) = send(s, "GET", &format!("/api/projects/{project}/queue"), b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

fn started(s: &Setup) -> Vec<String> {
    s.launcher.started.lock().unwrap().clone()
}

/// The next launch finds another run of the project at `repo` started meanwhile, and fails with `why`.
fn race(s: &Setup, repo: &Path, why: LaunchError) {
    *s.launcher.fail.lock().unwrap() = Some(why);
    let (state, repo) = (s.roots.state.clone(), repo.to_path_buf());
    *s.launcher.meanwhile.lock().unwrap() = Some(Box::new(move || seed_run(&state, &repo, "run-other", "OTHER")));
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
            occurred_at: format!("2026-11-19T10:00:{n:02}Z"),
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

/// Ends `run` with `ending` (`review_ready`, `failed`, ...).
fn end_run(s: &Setup, run: &str, ending: &str) {
    event(&RunStore::new(&s.roots.state), run, "run.completed", json!({"ending": ending}));
}

/// The queue is not paused and `first` is still its first entry, not started.
fn waits(s: &Setup, project: &str, first: &str) {
    let q = queue(s, project);
    assert_eq!(q["paused"], Value::Null, "a run limit reached by the launch itself is waited out: {q}");
    assert_eq!((&q["entries"][0]["feature"], &q["entries"][0]["run"]), (&json!(first), &Value::Null), "{q}");
}

#[test]
fn a_start_failing_on_the_project_limit_reached_meanwhile_waits_for_a_slot() {
    let s = setup("qsrl-project", "");
    let alpha = git_repo(&s, "alpha", &["A-01"], Some(1));
    queue_add(&s, "alpha", json!({"features": ["A-01"]}));
    race(&s, &alpha, LaunchError::Failed("alpha already has 1 active run (run-other, OTHER): wait for it to finish, or raise the limit in project settings".into()));

    round(&s);
    waits(&s, "alpha", "A-01");

    end_run(&s, "run-other", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["A-01"], "started once the slot freed");
}

#[test]
fn a_start_failing_on_the_machine_limit_reached_meanwhile_waits_for_a_slot() {
    let s = setup("qsrl-machine", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &["A-01"], None);
    let beta = git_repo(&s, "beta", &["B-01"], None);
    queue_add(&s, "alpha", json!({"features": ["A-01"]}));
    race(&s, &beta, LaunchError::Failed("this machine already has 1 active run (beta run-other): wait for it to finish, or raise [runs] max_active".into()));

    round(&s);
    waits(&s, "alpha", "A-01");

    end_run(&s, "run-other", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["A-01"], "started once the slot freed");
}

#[test]
fn a_queued_resume_failing_on_the_limit_reached_meanwhile_waits_for_a_slot() {
    let s = setup("qsrl-resume", "");
    let alpha = git_repo(&s, "alpha", &["A-01"], Some(1));
    seed_run(&s.roots.state, &alpha, "run-old", "A-01");
    end_run(&s, "run-old", "failed");
    queue_add(&s, "alpha", json!({"action": "resume", "run": "run-old"}));
    race(&s, &alpha, LaunchError::Failed("alpha already has 1 active run (run-other, OTHER): wait for it to finish, or raise the limit in project settings".into()));

    round(&s);
    let q = queue(&s, "alpha");
    assert_eq!(q["paused"], Value::Null, "a run limit reached by the resume itself is waited out: {q}");
    assert_eq!((&q["entries"][0]["kind"], &q["entries"][0]["run"]), (&json!("resume"), &Value::Null), "{q}");

    end_run(&s, "run-other", "review_ready");
    round(&s);
    assert_eq!(*s.launcher.resumed.lock().unwrap(), ["run-old"], "resumed once the slot freed");
}

#[test]
fn a_launch_error_that_is_not_a_run_limit_still_pauses_the_queue() {
    let s = setup("qsrl-other-error", "");
    git_repo(&s, "alpha", &["A-01"], Some(1));
    queue_add(&s, "alpha", json!({"features": ["A-01"]}));
    *s.launcher.fail.lock().unwrap() = Some(LaunchError::Failed("git worktree add failed".into()));

    round(&s);
    let reason = queue(&s, "alpha")["paused"].as_str().unwrap_or_default().to_string();
    assert_eq!(reason, "A-01 could not start; see the server's log");
    assert!(started(&s).is_empty());
}
