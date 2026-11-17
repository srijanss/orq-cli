//! A project's run limit and disk floor through the web: the settings API saves and checks both,
//! a start the project's limits refuse is a 409 with the reason and never reaches the launcher,
//! and the project listing says why a start would be refused (`start_blocked`).

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
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, feature: &str) -> Result<String, LaunchError> {
        self.started.lock().unwrap().push(feature.to_string());
        Ok("run-new".into())
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
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

fn save(s: &Setup, revision: u64, max_active_runs: Value, min_free_disk_gb: Value) -> (u16, Value) {
    let body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": max_active_runs, "min_free_disk_gb": min_free_disk_gb, "revision": revision});
    send(s, "POST", "/api/projects/alpha/settings", body.to_string().as_bytes())
}

fn listed(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects", b"");
    assert_eq!(status, 200, "{body}");
    body["projects"].as_array().unwrap().iter().find(|p| p["id"] == "alpha").cloned().unwrap_or_else(|| panic!("no alpha in {body}"))
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

/// A run of COM-02 in the project at `repo`, started and then blocked: not ended, so active.
fn blocked_run(s: &Setup, repo: &Path) -> (RunStore, RunRecord) {
    let store = RunStore::new(&s.roots.state);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-seed".into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: "COM-02".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: "orq/run-seed".into(),
        worktree: repo.join(".claude/worktrees/run-seed"),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.start_run(&record).unwrap();
    event(&store, &record, "stage.started", json!({}));
    event(&store, &record, "run.completed", json!({"ending": "blocked"}));
    (store, record)
}

#[test]
fn the_settings_api_saves_both_limits_and_refuses_values_out_of_range() {
    let s = setup("run-limit-settings");
    git_repo(&s, "alpha");
    let (status, body) = send(&s, "GET", "/api/projects/alpha/settings", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["settings"]["max_active_runs"], &body["settings"]["min_free_disk_gb"]), (&Value::Null, &Value::Null), "no limit unless set: {body}");

    let (status, body) = save(&s, 0, json!(1), json!(15));
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["settings"]["max_active_runs"], &body["settings"]["min_free_disk_gb"]), (&json!(1), &json!(15)), "{body}");
    assert_eq!(send(&s, "GET", "/api/projects/alpha/settings", b"").1, body, "read back as saved");

    for (max, floor) in [(json!(0), Value::Null), (json!(9), Value::Null), (Value::Null, json!(0)), (Value::Null, json!(501))] {
        let (status, refused) = save(&s, 1, max.clone(), floor.clone());
        assert_eq!(status, 422, "{max} {floor}: {refused}");
    }
    let (status, body) = save(&s, 1, Value::Null, Value::Null);
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["settings"]["max_active_runs"], &body["settings"]["min_free_disk_gb"]), (&Value::Null, &Value::Null), "cleared: {body}");
}

#[test]
fn a_start_at_the_limit_is_a_409_with_the_reason_and_the_listing_says_why_until_the_run_ends() {
    let s = setup("run-limit-start");
    let repo = git_repo(&s, "alpha");
    let (store, record) = blocked_run(&s, &repo);

    let project = listed(&s);
    assert_eq!((&project["active_runs"], &project["max_active_runs"], &project["start_blocked"]), (&json!(1), &Value::Null, &Value::Null), "no limit: {project}");

    assert_eq!(save(&s, 0, json!(1), Value::Null).0, 200);
    let reason = "alpha already has 1 active run (run-seed, COM-02): wait for it to finish, or raise the limit in project settings";
    let project = listed(&s);
    assert_eq!((&project["active_runs"], &project["max_active_runs"], &project["start_blocked"]), (&json!(1), &json!(1), &json!(reason)), "{project}");
    let (status, features) = send(&s, "GET", "/api/projects/alpha/features", b"");
    assert_eq!((status, &features["start_blocked"]), (200, &json!(reason)), "the feature page's reply says so too: {features}");

    assert_eq!(send(&s, "POST", "/api/projects/alpha/features/COM-01/runs", b""), (409, json!({"error": reason})));
    assert!(s.launcher.started.lock().unwrap().is_empty(), "a refused start never reaches the launcher");

    event(&store, &record, "run.resumed", json!({}));
    event(&store, &record, "run.completed", json!({"ending": "failed"}));
    assert_eq!(listed(&s)["start_blocked"], Value::Null);
    assert_eq!(send(&s, "GET", "/api/projects/alpha/features", b"").1["start_blocked"], Value::Null);
    assert_eq!(send(&s, "POST", "/api/projects/alpha/features/COM-01/runs", b""), (202, json!({"run": "run-new"})));
}

#[test]
fn under_the_disk_floor_the_listing_says_so_and_a_start_is_a_409_with_what_is_free_and_needed() {
    let s = setup("run-limit-disk");
    git_repo(&s, "alpha");
    assert_eq!(save(&s, 0, Value::Null, json!(15)).0, 200);
    // SAFETY: the only test in this binary that reads or writes the variable.
    unsafe { std::env::set_var("ORQ_TEST_FREE_DISK_BYTES", "9800000000") };
    let project = listed(&s);
    let started = send(&s, "POST", "/api/projects/alpha/features/COM-01/runs", b"");
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };

    let reason = project["start_blocked"].as_str().unwrap_or_else(|| panic!("{project}"));
    assert!(reason.starts_with("only 9.8 GB free on ") && reason.ends_with("(alpha needs 15 GB to start a run): free some space, or lower the floor in project settings"), "{reason}");
    assert_eq!(started, (409, json!({"error": reason})));
    assert!(s.launcher.started.lock().unwrap().is_empty());
}
