//! A project's run queue: approved features queued from the web or `orq queue`, started one by one by
//! the server's rounds through the web's own launcher, in dependency order and then the order added,
//! within the project's run limit, disk floor and queue hours, never while a restart is pending. A
//! completed run's entry leaves the queue; a failed one stays, marked failed, and only its dependents
//! wait. Each round is driven here with a fake launcher and a stated time of day.

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
    /// Done once, while the first run starts: what a person changes meanwhile.
    meanwhile: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        if let Some(refusal) = self.refuse.lock().unwrap().take() {
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

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default(), refuse: Mutex::default(), meanwhile: Mutex::default() });
    Setup { scan: base.join("code"), roots, launcher }
}

/// A repository `alpha` whose ledger holds `features` as (name, status, dependsOn).
fn git_repo(s: &Setup, features: &[(&str, &str, &[&str])]) -> PathBuf {
    let path = s.scan.join("alpha");
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

fn queue_add(s: &Setup, features: &[&str]) -> (u16, Value) {
    send(s, "POST", "/api/projects/alpha/queue", json!({"features": features}).to_string().as_bytes())
}

/// The queue as the page reads it.
fn queue(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// Each entry's feature and state, in queue order.
fn states(s: &Setup) -> Vec<(String, String)> {
    queue(s)["entries"].as_array().unwrap().iter().map(|e| (e["feature"].as_str().unwrap().to_string(), e["state"].as_str().unwrap().to_string())).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
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
fn a_feature_waits_in_the_queue_until_the_feature_it_depends_on_completes_then_starts() {
    let s = setup("queue-deps");
    let repo = git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &["COM-01"])]);
    // Added dependent first: the queue still runs the dependency first.
    assert_eq!(queue_add(&s, &["COM-02", "COM-01"]).0, 200);
    assert_eq!(states(&s), pairs(&[("COM-01", "next"), ("COM-02", "waiting")]));

    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(states(&s), pairs(&[("COM-01", "running"), ("COM-02", "waiting")]));
    assert_eq!(queue(&s)["entries"][0]["run"], "run-1-COM-01");

    round(&s);
    assert_eq!(started(&s), ["COM-01"], "COM-02 waits while COM-01 is not completed");

    end_run(&s, "run-1-COM-01", "merged");
    write_ledger(&repo, &[("COM-01", "completed", &[]), ("COM-02", "pending", &["COM-01"])]);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"]);
    assert_eq!(states(&s), pairs(&[("COM-02", "running")]), "a completed run's entry leaves the queue");
}

#[test]
fn a_pause_or_removal_made_while_a_round_starts_a_run_holds_the_starts_after_it() {
    let s = setup("queue-meanwhile");
    let repo = git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[]), ("COM-03", "pending", &[])]);
    assert_eq!(queue_add(&s, &["COM-01", "COM-02", "COM-03"]).0, 200);
    let database = s.roots.database();
    let root = repo.clone();
    *s.launcher.meanwhile.lock().unwrap() = Some(Box::new(move || {
        let store = orq_core::store::Store::open(&database).unwrap();
        store.queue_remove(&root, "COM-02").unwrap();
        store.queue_pause(&root, "paused from the web").unwrap();
    }));
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "nothing starts after the queue was paused");
    assert_eq!(states(&s), pairs(&[("COM-01", "running"), ("COM-03", "next")]));

    std::fs::write(s.roots.config_file(), "remote_actions = true\n").unwrap();
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/resume", b"{}").0, 200);
    let database = s.roots.database();
    let root = repo.clone();
    *s.launcher.meanwhile.lock().unwrap() = Some(Box::new(move || {
        orq_core::store::Store::open(&database).unwrap().queue_remove(&root, "COM-04").unwrap();
    }));
    write_ledger(&repo, &[("COM-01", "pending", &[]), ("COM-03", "pending", &[]), ("COM-04", "pending", &[]), ("COM-05", "pending", &[])]);
    assert_eq!(queue_add(&s, &["COM-04", "COM-05"]).0, 200);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-03", "COM-05"], "a feature removed meanwhile is not started");
}

/// Saves the project's settings from the web: no agents, review delivery, and `extra`.
fn save_settings(s: &Setup, extra: Value) {
    let (_, current) = send(s, "GET", "/api/projects/alpha/settings", b"");
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "revision": current["settings"]["revision"]});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let (status, reply) = send(s, "POST", "/api/projects/alpha/settings", body.to_string().as_bytes());
    assert_eq!(status, 200, "{reply}");
}

fn round_at(s: &Setup, minute_of_day: u16, restart_pending: bool) {
    provider(s).queue_round(&QueueRound { minute_of_day, restart_pending });
}

fn paused(s: &Setup) -> Value {
    queue(s)["paused"].clone()
}

#[test]
fn a_run_limit_of_one_holds_the_second_entry_until_the_first_run_ends() {
    let s = setup("queue-limit");
    git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[])]);
    save_settings(&s, json!({"max_active_runs": 1}));
    queue_add(&s, &["COM-01", "COM-02"]);

    round(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "one slot, one run");
    assert_eq!(states(&s), pairs(&[("COM-01", "running"), ("COM-02", "next")]));
    assert_eq!(paused(&s), Value::Null, "the run limit is waited out, never a pause");

    end_run(&s, "run-1-COM-01", "review_ready");
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"]);
}

#[test]
fn outside_the_queue_hours_nothing_starts_and_inside_them_the_queue_goes_on() {
    let s = setup("queue-hours");
    git_repo(&s, &[("COM-01", "pending", &[])]);
    save_settings(&s, json!({"queue_hours": "22:00-07:00"}));
    queue_add(&s, &["COM-01"]);

    round_at(&s, 12 * 60, false);
    round_at(&s, 7 * 60, false);
    assert!(started(&s).is_empty(), "noon and 07:00 are outside 22:00-07:00");
    round_at(&s, 23 * 60 + 30, false);
    assert_eq!(started(&s), ["COM-01"]);
}

#[test]
fn a_pending_restart_holds_every_start() {
    let s = setup("queue-restart");
    git_repo(&s, &[("COM-01", "pending", &[])]);
    queue_add(&s, &["COM-01"]);
    round_at(&s, 12 * 60, true);
    assert!(started(&s).is_empty());
    assert_eq!(paused(&s), Value::Null, "a restart only holds the queue");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
}

#[test]
fn a_start_that_fails_pauses_the_queue_with_the_reason_and_is_not_retried_until_resumed() {
    let s = setup("queue-launch-failure");
    git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[])]);
    queue_add(&s, &["COM-01", "COM-02"]);
    *s.launcher.refuse.lock().unwrap() = Some(LaunchError::Refused("the target branch main is checked out elsewhere".into()));

    round(&s);
    let reason = paused(&s);
    assert_eq!(reason, "COM-01 could not start: the target branch main is checked out elsewhere");
    round(&s);
    assert!(started(&s).is_empty(), "a paused queue starts nothing, COM-02 included");
    assert_eq!(states(&s), pairs(&[("COM-01", "next"), ("COM-02", "queued")]));

    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue/resume", b"{}");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["queue"]["paused"], Value::Null);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-02"]);
}

#[test]
fn a_failed_run_stays_marked_failed_its_dependents_never_start_and_the_rest_go_on() {
    let s = setup("queue-failed");
    let repo = git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &["COM-01"]), ("COM-03", "pending", &[])]);
    save_settings(&s, json!({"max_active_runs": 1}));
    queue_add(&s, &["COM-01", "COM-02", "COM-03"]);
    round(&s);
    end_run(&s, "run-1-COM-01", "failed");
    write_ledger(&repo, &[("COM-01", "in_progress", &[]), ("COM-02", "pending", &["COM-01"]), ("COM-03", "pending", &[])]);

    round(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01", "COM-03"], "COM-02 waits on the failed COM-01; COM-03 goes on");
    assert_eq!(paused(&s), Value::Null, "a failure does not pause by default");
    let entries = queue(&s)["entries"].clone();
    assert_eq!((&entries[0]["feature"], &entries[0]["state"], &entries[0]["run"]), (&json!("COM-01"), &json!("failed"), &json!("run-1-COM-01")));
    assert_eq!((&entries[1]["feature"], &entries[1]["state"]), (&json!("COM-02"), &json!("waiting")));
    assert_eq!(entries[1]["reason"], "waiting for COM-01");
}

#[test]
fn a_failed_run_resumed_as_another_is_followed_by_its_entry_until_that_run_completes() {
    let s = setup("queue-failed-resumed");
    let repo = git_repo(&s, &[("COM-01", "pending", &[])]);
    queue_add(&s, &["COM-01"]);
    round(&s);
    end_run(&s, "run-1-COM-01", "failed");
    round(&s);
    assert_eq!(states(&s), pairs(&[("COM-01", "failed")]));

    seed_run(&s.roots.state, &repo, "run-9-COM-01", "COM-01");
    event(&RunStore::new(&s.roots.state), "run-1-COM-01", "run.resumed", json!({"resumed_as": "run-9-COM-01"}));
    round(&s);
    assert_eq!(states(&s), pairs(&[("COM-01", "running")]), "the resumed run is followed");
    assert_eq!(queue(&s)["entries"][0]["run"], "run-9-COM-01");

    end_run(&s, "run-9-COM-01", "merged");
    round(&s);
    assert_eq!(states(&s), pairs(&[]), "its completion takes the entry out of the queue");
    assert_eq!(started(&s), ["COM-01"], "nothing was started again");
}

#[test]
fn with_stop_on_failure_a_failed_run_pauses_the_queue() {
    let s = setup("queue-stop-on-failure");
    git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[])]);
    save_settings(&s, json!({"max_active_runs": 1, "queue_stop_on_failure": true}));
    queue_add(&s, &["COM-01", "COM-02"]);
    round(&s);
    end_run(&s, "run-1-COM-01", "blocked");

    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(paused(&s), "the run run-1-COM-01 of COM-01 failed");
    assert_eq!(states(&s), pairs(&[("COM-01", "failed"), ("COM-02", "next")]));
}

#[test]
fn only_an_approved_feature_can_be_queued() {
    let s = setup("queue-draft");
    git_repo(&s, &[("COM-01", "draft", &[]), ("COM-02", "pending", &[])]);
    let (status, body) = queue_add(&s, &["COM-02", "COM-01"]);
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "COM-01 is draft: only an approved (pending) feature can be queued");
    assert_eq!(queue(&s)["entries"], json!([]), "nothing of a refused request is queued");
    assert_eq!(queue_add(&s, &["COM-09"]).0, 409, "a feature the ledger lacks");
}

#[test]
fn a_disk_floor_refusal_pauses_the_queue_with_its_reason() {
    let s = setup("queue-disk");
    git_repo(&s, &[("COM-01", "pending", &[])]);
    save_settings(&s, json!({"min_free_disk_gb": 15}));
    queue_add(&s, &["COM-01"]);
    // SAFETY: the only test in this binary that reads or writes the variable.
    unsafe { std::env::set_var("ORQ_TEST_FREE_DISK_BYTES", "9800000000") };
    round(&s);
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };
    assert!(started(&s).is_empty());
    let reason = paused(&s);
    let reason = reason.as_str().unwrap_or_else(|| panic!("{reason}"));
    assert!(reason.starts_with("only 9.8 GB free on ") && reason.ends_with("(alpha needs 15 GB to start a run): free some space, or lower the floor in project settings"), "{reason}");
}

#[test]
fn changing_the_queue_needs_remote_actions_and_every_change_is_audited() {
    let s = setup("queue-audit");
    git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[])]);
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(queue_add(&s, &["COM-01"]).0, 404);
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/pause", b"{}").0, 404);
    let (status, read) = send(&s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!((status, &read["remote_actions"]), (200, &json!(false)), "the queue is read with actions off, and says they are: {read}");
    std::fs::write(s.roots.config_file(), "remote_actions = true\n").unwrap();
    assert_eq!(send(&s, "GET", "/api/projects/alpha/queue", b"").1["remote_actions"], json!(true));
    assert_eq!(queue(&s)["entries"], json!([]), "the refused add queued nothing");

    assert_eq!(queue_add(&s, &["COM-01", "COM-02"]).0, 200);
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue/reorder", br#"{"order": ["COM-02", "COM-01"]}"#);
    assert_eq!(status, 200, "{body}");
    assert_eq!(states(&s), pairs(&[("COM-02", "next"), ("COM-01", "queued")]));
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/reorder", br#"{"order": ["COM-02"]}"#).0, 409, "an order that leaves an entry out");
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/pause", b"{}").0, 200);
    assert_eq!(paused(&s), "paused from the web");
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/remove", br#"{"feature": "COM-01"}"#).0, 200);
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue/remove", br#"{"feature": "COM-01"}"#).0, 409);
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue", br#"{"features": ["../x"]}"#).0, 400);
    assert_eq!(send(&s, "POST", "/api/projects/alpha/queue", br#"{"features": ["COM-01"], "extra": 1}"#).0, 400);

    let audit: Vec<(String, String)> = orq_core::remote_audit::read(&s.roots, 50)
        .unwrap()
        .into_iter()
        .rev()
        .filter(|line| line["action"].as_str().is_some_and(|a| a.starts_with("queue.")))
        .map(|line| (line["action"].as_str().unwrap().to_string(), line["outcome"].as_str().unwrap().to_string()))
        .collect();
    let expected = [
        ("queue.add", "refused"),
        ("queue.pause", "refused"),
        ("queue.add", "allowed"),
        ("queue.reorder", "allowed"),
        ("queue.reorder", "refused"),
        ("queue.pause", "allowed"),
        ("queue.remove", "allowed"),
        ("queue.remove", "refused"),
        ("queue.add", "refused"),
        ("queue.add", "refused"),
    ];
    assert_eq!(audit, pairs(&expected));
}

#[test]
fn the_dashboard_card_counts_the_queue_and_says_when_it_is_paused() {
    let s = setup("queue-card");
    git_repo(&s, &[("COM-01", "pending", &[]), ("COM-02", "pending", &[])]);
    queue_add(&s, &["COM-01", "COM-02"]);
    send(&s, "POST", "/api/projects/alpha/queue/pause", b"{}");
    let (status, body) = send(&s, "GET", "/api/projects", b"");
    assert_eq!(status, 200, "{body}");
    let card = body["projects"].as_array().unwrap().iter().find(|p| p["id"] == "alpha").cloned().unwrap();
    assert_eq!(card["queue"], json!({"count": 2, "paused": true}));
}

#[test]
fn the_server_s_queue_rounds_start_a_queued_feature_on_their_own_and_stop_when_dropped() {
    let s = setup("queue-rounds");
    git_repo(&s, &[("COM-01", "pending", &[])]);
    queue_add(&s, &["COM-01"]);
    let rounds = orq_remote::projects::start_queue_rounds(std::sync::Arc::new(provider(&s)), std::time::Duration::from_millis(20), || false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while started(&s).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(rounds);
    assert_eq!(started(&s), ["COM-01"]);
}
