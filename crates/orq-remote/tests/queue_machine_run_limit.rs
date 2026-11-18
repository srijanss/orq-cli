//! The run queues under this machine's `[runs]` limits: a full machine is waited out, never a
//! pause, and when slots are scarce the oldest waiting entry across every project starts first; a
//! machine disk floor the start falls under pauses the queue with the machine's reason.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Starts a run the way `orq run` records one, or refuses with the reason it was given.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<String>>,
    refuse: Mutex<Option<LaunchError>>,
    /// Done once, while the first run starts or is refused: what changes meanwhile.
    meanwhile: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        if let Some(refusal) = self.refuse.lock().unwrap().take() {
            // Another start took the last slot between the queue's check and this one's.
            if let Some(change) = self.meanwhile.lock().unwrap().take() {
                change();
            }
            return Err(refusal);
        }
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature);
        drop(started);
        if let Some(change) = self.meanwhile.lock().unwrap().take() {
            change();
        }
        Ok(run)
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
}

fn setup(label: &str, runs: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), format!("remote_actions = true\n{runs}")).unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default(), refuse: Mutex::default(), meanwhile: Mutex::default() });
    Setup { scan: base.join("code"), roots, launcher }
}

/// A repository `name` whose ledger holds `features` as (name, status, dependsOn).
fn git_repo(s: &Setup, name: &str, features: &[(&str, &str, &[&str])]) -> PathBuf {
    let path = s.scan.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let path = std::fs::canonicalize(path).unwrap();
    write_ledger(&path, features);
    path
}

fn write_ledger(repo: &Path, features: &[(&str, &str, &[&str])]) {
    let ledger: Vec<Value> = features.iter().map(|(name, status, deps)| json!({"featureName": name, "dependsOn": deps, "status": status})).collect();
    std::fs::write(repo.join(".tdd-features.json"), Value::Array(ledger).to_string()).unwrap();
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
    let (status, body) = send(s, "POST", &format!("/api/projects/{project}/queue"), json!({"features": features}).to_string().as_bytes());
    assert_eq!(status, 200, "{body}");
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
            occurred_at: "2026-11-18T10:00:00Z".into(),
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

/// Ends `run` with `ending` (`merged`, `failed`, `blocked`, ...).
fn end_run(s: &Setup, run: &str, ending: &str) {
    event(&RunStore::new(&s.roots.state), run, "run.completed", json!({"ending": ending}));
}

#[test]
fn on_a_full_machine_the_queues_wait_and_the_oldest_waiting_entry_of_any_project_starts_next() {
    let s = setup("queue-machine-full", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &[("A-01", "pending", &[]), ("A-02", "pending", &[])]);
    git_repo(&s, "beta", &[("B-01", "pending", &[])]);
    queue_add(&s, "beta", &["B-01"]);
    // Added a second later, alpha's entries come after beta's though alpha is listed first.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    queue_add(&s, "alpha", &["A-01", "A-02"]);

    round(&s);
    round(&s);
    assert_eq!(started(&s), ["B-01"], "one slot on the machine, the oldest entry");
    assert_eq!((queue(&s, "alpha")["paused"].clone(), queue(&s, "beta")["paused"].clone()), (Value::Null, Value::Null), "a full machine is waited out");

    end_run(&s, "run-1-B-01", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["B-01", "A-01"]);
    end_run(&s, "run-2-A-01", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["B-01", "A-01", "A-02"]);
}

#[test]
fn under_the_machines_disk_floor_the_queue_pauses_with_the_machines_reason() {
    let s = setup("queue-machine-disk", "[runs]\nmin_free_disk_gb = 15\n");
    git_repo(&s, "alpha", &[("A-01", "pending", &[])]);
    queue_add(&s, "alpha", &["A-01"]);
    // SAFETY: the only test in this binary that sets it.
    unsafe { std::env::set_var("ORQ_TEST_FREE_DISK_BYTES", "9800000000") };
    round(&s);
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };
    assert!(started(&s).is_empty());
    let reason = queue(&s, "alpha")["paused"].as_str().unwrap_or_default().to_string();
    assert!(reason.contains("(this machine needs 15 GB to start a run): free some space, or lower [runs] min_free_disk_gb"), "{reason}");
}

#[test]
fn a_start_refused_because_another_took_the_last_machine_slot_meanwhile_is_waited_out() {
    let s = setup("queue-machine-race", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &[("A-01", "pending", &[])]);
    let beta = git_repo(&s, "beta", &[("B-01", "pending", &[])]);
    queue_add(&s, "alpha", &["A-01"]);
    *s.launcher.refuse.lock().unwrap() = Some(LaunchError::Refused("this machine already has 1 active run (beta run-other): wait for it to finish, or raise [runs] max_active".into()));
    let state = s.roots.state.clone();
    *s.launcher.meanwhile.lock().unwrap() = Some(Box::new(move || seed_run(&state, &beta, "run-other", "B-01")));

    round(&s);
    let waiting = queue(&s, "alpha");
    assert_eq!(waiting["paused"], Value::Null, "a full machine is waited out, even when found by the start itself: {waiting}");

    end_run(&s, "run-other", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["A-01"]);
}

#[test]
fn an_older_entry_that_cannot_start_yet_does_not_put_its_project_first() {
    let s = setup("queue-machine-eligible", "[runs]\nmax_active = 1\n");
    git_repo(&s, "alpha", &[("A-00", "pending", &[]), ("A-01", "pending", &["A-00"]), ("A-02", "pending", &[])]);
    git_repo(&s, "beta", &[("B-01", "pending", &[])]);
    // A-01 is the oldest entry, but it waits on A-00, which is not queued.
    queue_add(&s, "alpha", &["A-01"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    queue_add(&s, "beta", &["B-01"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    queue_add(&s, "alpha", &["A-02"]);

    round(&s);
    assert_eq!(started(&s), ["B-01"], "the oldest entry that can start goes first");
}
