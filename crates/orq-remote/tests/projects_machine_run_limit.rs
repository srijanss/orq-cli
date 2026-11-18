//! The machine's run limit and disk floor (`[runs]` in this machine's config) through the web: the
//! project listing carries the machine's active runs, limits and free disk, each project's
//! `start_blocked` gives the machine's reason, and a start or a blocked run's resume the machine
//! refuses is a 409 that never reaches the launcher, while a paused run's resume is not counted.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

#[derive(Default)]
struct Launcher {
    started: Mutex<Vec<String>>,
    resumed: Mutex<Vec<String>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, feature: &str) -> Result<String, LaunchError> {
        self.started.lock().unwrap().push(feature.to_string());
        Ok("run-new".into())
    }

    fn resume(&self, _repo: &Path, run: &str) -> Result<String, LaunchError> {
        self.resumed.lock().unwrap().push(run.to_string());
        Ok("run-resumed".into())
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
    Setup { scan: base.join("code"), roots, launcher: Arc::new(Launcher::default()) }
}

fn git_repo(s: &Setup, name: &str) -> PathBuf {
    let path = s.scan.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let ledger = json!([
        {"featureName": "COM-01", "dependsOn": [], "status": "pending"},
        {"featureName": "COM-02", "dependsOn": [], "status": "pending"}
    ]);
    std::fs::write(path.join(".tdd-features.json"), ledger.to_string()).unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider);
    (r.status, r.body)
}

fn listing(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects", b"");
    assert_eq!(status, 200, "{body}");
    body
}

fn listed<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["projects"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap_or_else(|| panic!("no {id} in {body}"))
}

fn event(store: &RunStore, record: &RunRecord, kind: &str, payload: Value) {
    let n = store.events(&record.run_id).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{}-{n}", record.run_id),
            run_id: record.run_id.clone(),
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

/// A run of `feature` in the project at `repo`, started.
fn seeded_run(s: &Setup, repo: &Path, run_id: &str, feature: &str) -> (RunStore, RunRecord) {
    let store = RunStore::new(&s.roots.state);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run_id.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: format!("orq/{run_id}"),
        worktree: repo.join(".claude/worktrees").join(run_id),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, &record, "stage.started", json!({}));
    (store, record)
}

const MACHINE_FULL: &str = "this machine already has 1 active run (alpha run-seed): wait for it to finish, or raise [runs] max_active";

#[test]
fn on_a_full_machine_the_listing_says_so_and_a_start_in_another_project_is_a_409_with_the_machines_reason() {
    let s = setup("machine-full", "[runs]\nmax_active = 1\n");
    let alpha = git_repo(&s, "alpha");
    git_repo(&s, "beta");
    let body = listing(&s);
    let machine = &body["machine"];
    assert_eq!((&machine["active_runs"], &machine["max_active_runs"], &machine["min_free_disk_gb"]), (&json!(0), &json!(1), &Value::Null), "{body}");
    assert!(machine["free_disk_gb"].is_number(), "free disk is shown with a limit: {body}");

    let (store, seed) = seeded_run(&s, &alpha, "run-seed", "COM-02");
    let body = listing(&s);
    assert_eq!(body["machine"]["active_runs"], json!(1), "{body}");
    assert_eq!(listed(&body, "beta")["start_blocked"], json!(MACHINE_FULL), "{body}");
    assert_eq!(listed(&body, "alpha")["start_blocked"], json!(MACHINE_FULL), "{body}");
    assert_eq!(send(&s, "POST", "/api/projects/beta/features/COM-01/runs", b""), (409, json!({"error": MACHINE_FULL})));
    assert_eq!(send(&s, "GET", "/api/projects/beta/features", b"").1["start_blocked"], json!(MACHINE_FULL));
    assert!(s.launcher.started.lock().unwrap().is_empty(), "a refused start never reaches the launcher");

    event(&store, &seed, "run.completed", json!({"ending": "review_ready"}));
    assert_eq!(listed(&listing(&s), "beta")["start_blocked"], Value::Null);
    assert_eq!(send(&s, "POST", "/api/projects/beta/features/COM-01/runs", b""), (202, json!({"run": "run-new"})));
}

#[test]
fn without_runs_settings_the_listing_has_no_machine_line_and_starts_are_free() {
    let s = setup("machine-unset", "");
    let alpha = git_repo(&s, "alpha");
    git_repo(&s, "beta");
    seeded_run(&s, &alpha, "run-seed", "COM-02");
    let body = listing(&s);
    assert_eq!(body["machine"], Value::Null, "{body}");
    assert_eq!(listed(&body, "beta")["start_blocked"], Value::Null, "{body}");
}

#[test]
fn a_paused_runs_resume_is_not_counted_but_a_blocked_runs_is_refused_on_a_full_machine() {
    let s = setup("machine-resume", "[runs]\nmax_active = 1\n");
    let alpha = git_repo(&s, "alpha");
    let beta = git_repo(&s, "beta");
    let (store, paused) = seeded_run(&s, &alpha, "run-seed", "COM-02");
    event(&store, &paused, "run.pause_requested", json!({}));
    event(&store, &paused, "run.paused", json!({}));
    let (_, blocked) = seeded_run(&s, &beta, "run-blocked", "COM-01");
    event(&store, &blocked, "run.completed", json!({"ending": "blocked"}));

    assert_eq!(send(&s, "POST", "/api/projects/beta/runs/run-blocked/resume", b""), (409, json!({"error": MACHINE_FULL})));
    assert!(s.launcher.resumed.lock().unwrap().is_empty());
    assert_eq!(send(&s, "POST", "/api/projects/alpha/runs/run-seed/resume", b""), (202, json!({"run": "run-resumed"})));
}

#[test]
fn a_malformed_runs_setting_refuses_a_start_naming_the_key() {
    let s = setup("machine-malformed", "[runs]\nmax_active = 0\n");
    git_repo(&s, "alpha");
    let (status, body) = send(&s, "POST", "/api/projects/alpha/features/COM-01/runs", b"");
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().contains("[runs] max_active must be empty or a whole number of at least 1"), "{body}");
    assert!(s.launcher.started.lock().unwrap().is_empty());
}

#[test]
fn under_the_machines_disk_floor_the_listing_shows_the_free_disk_and_a_start_is_a_409() {
    let s = setup("machine-disk", "[runs]\nmin_free_disk_gb = 15\n");
    git_repo(&s, "alpha");
    // SAFETY: the only test in this binary that sets it.
    unsafe { std::env::set_var("ORQ_TEST_FREE_DISK_BYTES", "9800000000") };
    let body = listing(&s);
    let started = send(&s, "POST", "/api/projects/alpha/features/COM-01/runs", b"");
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };
    let machine = &body["machine"];
    assert_eq!((&machine["max_active_runs"], &machine["min_free_disk_gb"], &machine["free_disk_gb"]), (&Value::Null, &json!(15), &json!(9.8)), "{body}");
    let blocked = listed(&body, "alpha")["start_blocked"].as_str().unwrap_or_default().to_string();
    assert!(blocked.starts_with("only 9.8 GB free on ") && blocked.ends_with("(this machine needs 15 GB to start a run): free some space, or lower [runs] min_free_disk_gb"), "{body}");
    assert_eq!(started, (409, json!({"error": blocked})));
    assert!(s.launcher.started.lock().unwrap().is_empty());
}
