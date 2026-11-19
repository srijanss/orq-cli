//! Resume, Review again and Accept can be queued for a run as well as started now: the entry waits its
//! turn under the project's run limit like a feature's, is started through the same paths as the web's
//! Resume and Accept, and counts like a feature's run once it goes. An entry whose run no longer needs it
//! (resumed, accepted, discarded or merged meanwhile, or its feature started again) is marked "No longer
//! needed" and skipped. Each round is driven here with a fake launcher.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{AcceptTests, LaunchError, QueueRound, ResumeRequest, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// What the fake launcher was asked to resume: the run, the answer given, and whether it was a review again.
type Resumed = (String, Option<String>, bool);

/// Starts and resumes runs the way `orq run` records them.
struct Launcher {
    state: PathBuf,
    started: Mutex<Vec<String>>,
    resumed: Mutex<Vec<Resumed>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature);
        Ok(run)
    }

    fn resume_with(&self, repo: &Path, run: &str, request: &ResumeRequest) -> Result<String, LaunchError> {
        let mut resumed = self.resumed.lock().unwrap();
        resumed.push((run.to_string(), request.answer.map(str::to_string), request.review_again));
        let store = RunStore::new(&self.state);
        let feature = store.load(run).unwrap().feature_id;
        let next = format!("{run}-r{}", resumed.len());
        seed_run(&self.state, repo, &next, &feature);
        event(&store, run, "run.resumed", json!({"resumed_as": next}));
        Ok(next)
    }
}

struct Setup {
    roots: StorageRoots,
    launcher: Arc<Launcher>,
    repo: PathBuf,
    /// One provider for the whole test, as the server keeps one: it knows which runs it is accepting.
    projects: StoreProjects,
    /// What each accept's tests end with, taken in turn; an accept's tests wait until one is given.
    accept_outcomes: Arc<Mutex<Vec<Result<(), String>>>>,
}

/// What an accept's tests end with, taken in turn.
type AcceptOutcomes = Arc<Mutex<Vec<Result<(), String>>>>;

/// A project `alpha` with approved features COM-01 and COM-02 and COM-00 in progress, at most one run
/// active, and `settings` changed further by `change`. Nothing is queued yet.
fn setup(label: &str, change: impl FnOnce(&mut ProjectSettings)) -> Setup {
    setup_with(label, change, |projects, _| projects)
}

/// [`setup`], with the provider changed further by `extra`, which is given the accepts' outcomes.
fn setup_with(label: &str, change: impl FnOnce(&mut ProjectSettings), extra: impl FnOnce(StoreProjects, AcceptOutcomes) -> StoreProjects) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default(), resumed: Mutex::default() });
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let repo = std::fs::canonicalize(path).unwrap();
    let ledger = json!([
        {"featureName": "COM-00", "dependsOn": [], "status": "in_progress"},
        {"featureName": "COM-01", "dependsOn": [], "status": "pending"},
        {"featureName": "COM-02", "dependsOn": [], "status": "pending"},
    ]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let mut settings = ProjectSettings { delivery: Delivery::MergeLocal, max_active_runs: Some(1), ..ProjectSettings::default() };
    change(&mut settings);
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let accept_outcomes = Arc::new(Mutex::new(Vec::<Result<(), String>>::new()));
    let outcomes = accept_outcomes.clone();
    let tests: AcceptTests = Arc::new(move |_: &Path| loop {
        if let Some(outcome) = { let mut given = outcomes.lock().unwrap(); (!given.is_empty()).then(|| given.remove(0)) } {
            return outcome;
        }
        std::thread::sleep(Duration::from_millis(10));
    });
    let projects = extra(StoreProjects::new(roots.clone(), scan).with_launcher(launcher.clone()).with_accept_tests(tests), accept_outcomes.clone());
    Setup { roots, launcher, repo, projects, accept_outcomes }
}

/// Lets the accept whose tests are running end with `outcome`, then works the queue until the
/// Accept entry of `run` has seen it end (the accept lets go of its run just after its last event).
fn finish_accept(s: &Setup, run: &str, outcome: Result<(), String>) {
    s.accept_outcomes.lock().unwrap().push(outcome);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        round(s);
        let going = queue(s)["entries"].as_array().unwrap().iter().any(|e| e["kind"] == "accept" && e["target_run"] == run && e["state"] == "running");
        if !going {
            return;
        }
        assert!(Instant::now() < until, "the accept of {run} did not end");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &s.projects);
    (r.status, r.body)
}

/// Adds to the queue as the page does, and checks it was added.
fn add(s: &Setup, body: Value) {
    let (status, answer) = send(s, "POST", "/api/projects/alpha/queue", body.to_string().as_bytes());
    assert_eq!(status, 200, "{answer}");
}

/// The queue as the page reads it.
fn queue(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

/// Each entry's kind, what it is for (feature or run) and state, in queue order.
fn states(s: &Setup) -> Vec<(String, String, String)> {
    queue(s)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let kind = e["kind"].as_str().unwrap().to_string();
            let of = e["target_run"].as_str().or(e["feature"].as_str()).unwrap().to_string();
            (kind, of, e["state"].as_str().unwrap().to_string())
        })
        .collect()
}

fn triples(list: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    list.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    s.projects.queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

fn started(s: &Setup) -> Vec<String> {
    s.launcher.started.lock().unwrap().clone()
}

fn resumed(s: &Setup) -> Vec<Resumed> {
    s.launcher.resumed.lock().unwrap().clone()
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
            occurred_at: format!("2026-11-18T10:00:{n:02}Z"),
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
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: git(repo, &["rev-parse", "HEAD"]),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

/// Runs git in `repo` and gives back what it printed, trimmed.
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(repo).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Ends `run` with `ending` (`review_ready`, `needs_review`, `failed`, `merged`, ...).
fn end_run(s: &Setup, run: &str, ending: &str) {
    event(&RunStore::new(&s.roots.state), run, "run.completed", json!({"ending": ending}));
}

/// A run `run` of `feature` whose worktree is a real checkout of its own branch with one commit of
/// work, ended with `ending`, as the web's Accept needs it.
fn worktree_run(s: &Setup, run: &str, feature: &str, ending: &str) {
    let worktree = s.repo.join(".claude/worktrees").join(run);
    git(&s.repo, &["worktree", "add", "-q", "-b", &format!("orq/{run}"), worktree.to_str().unwrap()]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", &format!("work of {run}")]);
    seed_run(&s.roots.state, &s.repo, run, feature);
    end_run(s, run, ending);
}

/// [`worktree_run`] for a merge-local run, its worktree outside the project's checkout, its one commit
/// adding `work.txt`.
fn merge_local_run(s: &Setup, run: &str, feature: &str, ending: &str) -> PathBuf {
    let worktree = s.repo.parent().unwrap().join(format!("{run}-worktree"));
    git(&s.repo, &["worktree", "add", "-q", "-b", &format!("orq/{run}"), worktree.to_str().unwrap()]);
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-q", "-m", &format!("work of {run}")]);
    let store = RunStore::new(&s.roots.state);
    let record = RunRecord {
        delivery: Delivery::MergeLocal,
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(&s.repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: git(&s.repo, &["rev-parse", "HEAD"]),
        branch: format!("orq/{run}"),
        worktree: worktree.clone(),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
    end_run(s, run, ending);
    worktree
}

/// A run `run` of COM-00 that ended with `ending`.
fn stopped_run(s: &Setup, run: &str, ending: &str) {
    seed_run(&s.roots.state, &s.repo, run, "COM-00");
    end_run(s, run, ending);
}

#[test]
fn a_queued_resume_of_a_failed_run_starts_only_after_the_running_one_ends_with_its_answer() {
    let s = setup("qra-resume", |_| {});
    stopped_run(&s, "run-old", "failed");
    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);

    add(&s, json!({"action": "resume", "run": "run-old", "answer": "use the v2 endpoint"}));
    round(&s);
    assert_eq!(resumed(&s), Vec::<Resumed>::new(), "the resume waits while COM-01 holds the one slot");
    assert_eq!(states(&s), triples(&[("start", "COM-01", "running"), ("resume", "run-old", "next")]));

    end_run(&s, "run-1-COM-01", "merged");
    round(&s);
    assert_eq!(resumed(&s), [("run-old".to_string(), Some("use the v2 endpoint".to_string()), false)]);
    assert_eq!(states(&s), triples(&[("resume", "run-old", "running")]), "the resume counts as the one active run");
    assert_eq!(queue(&s)["entries"][0]["run"], "run-old-r1", "the entry follows the run that carries on");

    end_run(&s, "run-old-r1", "merged");
    round(&s);
    assert_eq!(states(&s), Vec::<(String, String, String)>::new(), "a resumed run that merges leaves the queue");
}

#[test]
fn a_queued_accept_of_a_needs_review_run_accepts_it_in_turn_and_holds_the_slot_while_its_tests_run() {
    let s = setup("qra-accept", |_| {});
    worktree_run(&s, "run-nr", "COM-00", "needs_review");
    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    add(&s, json!({"action": "accept", "run": "run-nr"}));
    add(&s, json!({"features": ["COM-02"]}));
    round(&s);
    assert_eq!(states(&s), triples(&[("start", "COM-01", "running"), ("accept", "run-nr", "next"), ("start", "COM-02", "queued")]));

    end_run(&s, "run-1-COM-01", "merged");
    round(&s);
    assert_eq!(states(&s), triples(&[("accept", "run-nr", "running"), ("start", "COM-02", "next")]), "the accept's tests hold the one slot");
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(queue(&s)["entries"][0]["label"], "Accept run-nr");

    finish_accept(&s, "run-nr", Ok(()));
    assert_eq!(states(&s), triples(&[("start", "COM-02", "running")]), "an accepted run leaves the queue and the next one starts");
    assert_eq!(started(&s), ["COM-01", "COM-02"]);
}

#[test]
fn a_queued_accept_of_a_merge_local_run_merges_it_and_leaves_the_queue_once_it_is_merged() {
    let s = setup("qra-accept-merge-local", |_| {});
    let worktree = merge_local_run(&s, "run-ml", "COM-00", "needs_review");
    add(&s, json!({"action": "accept", "run": "run-ml"}));
    round(&s);
    assert_eq!(states(&s), triples(&[("accept", "run-ml", "running")]));

    finish_accept(&s, "run-ml", Ok(()));

    assert_eq!(states(&s), Vec::<(String, String, String)>::new(), "a merged run leaves the queue");
    assert_eq!(git(&s.repo, &["log", "-1", "--format=%s", "main"]), "work of run-ml", "main is fast-forwarded to the run's work");
    assert!(!worktree.exists(), "the merged run's worktree is removed");
    let events = RunStore::new(&s.roots.state).events("run-ml").unwrap();
    let completed = events.iter().rev().find(|e| e.kind == "run.completed").unwrap();
    assert_eq!(completed.payload["ending"], "merged", "{}", completed.payload);
    assert_eq!(completed.payload["note"], "accepted: tests passed, merged into main");
    let ledger: Value = serde_json::from_str(&std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap()).unwrap();
    assert_eq!(ledger[0]["featureName"], "COM-00");
    assert_eq!(ledger[0]["status"], "completed", "the merged run's feature is completed: {ledger}");
}

#[test]
fn a_merge_local_run_accepted_before_accepting_merged_shows_it_waits_to_merge_and_accept_merges_it() {
    let s = setup("qra-merge-accepted", |_| {});
    let worktree = merge_local_run(&s, "run-acc", "COM-00", "needs_review");
    let store = RunStore::new(&s.roots.state);
    // How an accept ended a merge-local run before accepting merged it.
    event(&store, "run-acc", "run.accept_started", json!({"by": "web", "head": git(&worktree, &["rev-parse", "HEAD"])}));
    event(&store, "run-acc", "run.completed", json!({"ending": "review_ready", "note": "accepted: tests passed", "by": "web"}));
    let run = |s: &Setup| send(s, "GET", "/api/projects/alpha/runs/run-acc", b"").1["run"].clone();
    assert_eq!(run(&s)["merge_pending"], true, "{}", run(&s));

    let (status, answer) = send(&s, "POST", "/api/projects/alpha/runs/run-acc/accept", b"");
    assert_eq!((status, answer), (202, json!({"run": "run-acc"})));
    // No accept outcome is given: tests run again would wait for one, and the merge would not end.
    let until = Instant::now() + Duration::from_secs(10);
    while run(&s)["ending"] != "merged" {
        assert!(Instant::now() < until, "the accepted run was not merged: {}", run(&s));
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(run(&s).get("merge_pending"), None, "a merged run has nothing left to merge");
    assert_eq!(git(&s.repo, &["log", "-1", "--format=%s", "main"]), "work of run-acc");
    assert!(!worktree.exists(), "the merged run's worktree is removed");
}

#[test]
fn a_queued_review_again_resumes_the_run_to_review_it_again() {
    let s = setup("qra-again", |_| {});
    stopped_run(&s, "run-nr", "needs_review");
    // Of another feature: a later run of the same feature would make it no longer needed.
    seed_run(&s.roots.state, &s.repo, "run-f", "COM-02");
    end_run(&s, "run-f", "failed");
    add(&s, json!({"action": "review_again", "run": "run-nr"}));
    round(&s);
    assert_eq!(resumed(&s), [("run-nr".to_string(), None, true)]);
    assert_eq!(queue(&s)["entries"][0]["label"], "Review again run-nr");
    add(&s, json!({"action": "review_again", "run": "run-f"}));
    end_run(&s, "run-nr-r1", "merged");
    round(&s);
    assert_eq!(resumed(&s)[1], ("run-f".to_string(), None, true), "a failed run is reviewed again too");
}

#[test]
fn an_entry_whose_run_was_resumed_by_hand_is_marked_no_longer_needed_and_skipped() {
    let s = setup("qra-skip", |_| {});
    stopped_run(&s, "run-old", "failed");
    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    add(&s, json!({"action": "resume", "run": "run-old"}));
    add(&s, json!({"features": ["COM-02"]}));
    // Resumed by hand meanwhile, as `orq run resume` records it.
    seed_run(&s.roots.state, &s.repo, "run-hand", "COM-00");
    end_run(&s, "run-hand", "failed");
    event(&RunStore::new(&s.roots.state), "run-old", "run.resumed", json!({"resumed_as": "run-hand"}));

    end_run(&s, "run-1-COM-01", "merged");
    round(&s);
    assert_eq!(resumed(&s), Vec::<Resumed>::new());
    assert_eq!(states(&s), triples(&[("resume", "run-old", "skipped"), ("start", "COM-02", "running")]), "the skipped entry is passed over");
    assert_eq!(queue(&s)["entries"][0]["reason"], "No longer needed: run-old was resumed as run-hand");
}

#[test]
fn a_queued_action_s_failure_pauses_the_queue_when_stop_on_failure_is_on() {
    let s = setup("qra-fail", |settings| settings.queue_stop_on_failure = true);
    stopped_run(&s, "run-old", "failed");
    add(&s, json!({"action": "resume", "run": "run-old", "answer": "try again"}));
    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    end_run(&s, "run-old-r1", "failed");
    round(&s);
    assert_eq!(states(&s), triples(&[("resume", "run-old", "failed"), ("start", "COM-01", "next")]));
    assert_eq!(queue(&s)["paused"], "the run run-old-r1 of Resume run-old failed");
    assert_eq!(started(&s), Vec::<String>::new(), "nothing starts while the queue is paused");
}

#[test]
fn an_action_on_a_run_that_is_not_the_project_s_or_that_it_does_not_apply_to_is_refused() {
    let s = setup("qra-refuse", |_| {});
    seed_run(&s.roots.state, &s.repo, "run-going", "COM-00");
    let refused = |body: Value| send(&s, "POST", "/api/projects/alpha/queue", body.to_string().as_bytes());
    assert_eq!(refused(json!({"action": "resume", "run": "nope"})), (409, json!({"error": "nope is not one of the project's runs"})));
    assert_eq!(refused(json!({"action": "accept", "run": "run-going"})), (409, json!({"error": "Accept run-going cannot be queued: run-going is running"})));
    assert_eq!(refused(json!({"action": "resume", "run": "../x"})).0, 400, "a run id is never a path");
    assert_eq!(refused(json!({"action": "accept", "run": "run-going", "answer": "x"})).0, 400, "only a Resume takes an answer");
    assert_eq!(refused(json!({"action": "start", "run": "run-going"})).0, 400);
    assert_eq!(states(&s), Vec::<(String, String, String)>::new());
}

#[test]
fn run_now_resume_takes_an_answer_and_review_again_resumes_a_needs_review_or_failed_run_to_review_it() {
    let s = setup("qra-now", |settings| settings.max_active_runs = Some(3));
    stopped_run(&s, "run-old", "failed");
    seed_run(&s.roots.state, &s.repo, "run-nr", "COM-01");
    end_run(&s, "run-nr", "needs_review");
    seed_run(&s.roots.state, &s.repo, "run-going", "COM-02");
    let post = |path: &str, body: Value| send(&s, "POST", &format!("/api/projects/alpha/runs/{path}"), if body.is_null() { Vec::new() } else { body.to_string().into_bytes() }.as_slice());

    assert_eq!(post("run-old/resume", json!({"answer": "  use the v2 endpoint "})), (202, json!({"run": "run-old-r1"})));
    assert_eq!(post("run-nr/review-again", Value::Null), (202, json!({"run": "run-nr-r2"})));
    assert_eq!(resumed(&s), [("run-old".to_string(), Some("use the v2 endpoint".to_string()), false), ("run-nr".to_string(), None, true)]);
    assert_eq!(post("run-going/review-again", Value::Null), (409, json!({"error": "run-going is running: only a run that needs review or failed can be reviewed again"})));
    assert_eq!(post("run-old-r1/review-again", json!({"answer": "x"})).0, 400, "a review again takes no answer");
    assert_eq!(resumed(&s).len(), 2);
}

/// Writes a review ledger in `worktree` with one round from `start` that reviewed `head`.
fn reviewed_up_to(worktree: &Path, start: &str, head: &str) {
    let scope = format!("review:{start}");
    let round = json!({"start": start, "head": head, "reviewer": "pi", "model": "m", "findings": [], "round": 1, "recordedAt": "2026-11-18T10:00:00+00:00"});
    std::fs::write(worktree.join(".tdd-review-rounds.json"), json!({ &scope: [round] }).to_string()).unwrap();
    std::fs::write(worktree.join(".tdd-review-findings.json"), json!({ &scope: [] }).to_string()).unwrap();
}

#[test]
fn a_failed_run_s_page_says_it_can_be_reviewed_again_only_once_something_came_after_its_last_review() {
    let s = setup("qra-reviewable", |_| {});
    worktree_run(&s, "run-f", "COM-01", "failed");
    let worktree = s.repo.join(".claude/worktrees/run-f");
    let base = git(&s.repo, &["rev-parse", "main"]);
    reviewed_up_to(&worktree, &base, &git(&worktree, &["rev-parse", "HEAD"]));
    let reviewable = |s: &Setup| send(s, "GET", "/api/projects/alpha/runs/run-f", b"").1["run"].get("reviewable_again").cloned();

    assert_eq!(reviewable(&s), None, "nothing came after its review");
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "after the review"]);
    assert_eq!(reviewable(&s), Some(json!(true)));
    end_run(&s, "run-f", "blocked");
    assert_eq!(reviewable(&s), None, "only a failed run");
}

#[test]
fn a_queued_resume_of_a_paused_run_starts_on_the_slot_the_paused_run_still_holds() {
    let s = setup("qra-paused", |_| {});
    seed_run(&s.roots.state, &s.repo, "run-p", "COM-00");
    event(&RunStore::new(&s.roots.state), "run-p", "run.pause_requested", json!({}));
    event(&RunStore::new(&s.roots.state), "run-p", "run.paused", json!({}));
    std::fs::write(s.roots.config_file(), "remote_actions = true\n[runs]\nmax_active = 1\n").unwrap();
    add(&s, json!({"action": "resume", "run": "run-p"}));
    round(&s);
    assert_eq!(resumed(&s), [("run-p".to_string(), None, false)], "the paused run's own slot is the one its resume carries on in");
}

/// A second project `name` next to alpha, with alpha's settings and features, as its main checkout.
fn second_project(s: &Setup, name: &str) -> PathBuf {
    let path = s.repo.parent().unwrap().join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["commit", "-q", "--allow-empty", "-m", "start"]);
    std::fs::copy(s.repo.join(".tdd-features.json"), path.join(".tdd-features.json")).unwrap();
    let settings = ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() };
    Store::open(&s.roots.database()).unwrap().save_project_settings(&path, 0, &settings).unwrap();
    path
}

#[test]
fn an_accept_queued_in_another_project_holds_a_machine_slot_while_its_tests_run() {
    let s = setup("qra-machine", |settings| settings.max_active_runs = None);
    std::fs::write(s.roots.config_file(), "remote_actions = true\n[runs]\nmax_active = 1\n").unwrap();
    let beta = second_project(&s, "beta");
    let worktree = beta.join(".claude/worktrees/run-b");
    git(&beta, &["worktree", "add", "-q", "-b", "orq/run-b", worktree.to_str().unwrap()]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "work of run-b"]);
    seed_run(&s.roots.state, &beta, "run-b", "COM-00");
    end_run(&s, "run-b", "needs_review");
    let (status, answer) = send(&s, "POST", "/api/projects/beta/queue", json!({"action": "accept", "run": "run-b"}).to_string().as_bytes());
    assert_eq!(status, 200, "{answer}");
    round(&s);
    assert_eq!(send(&s, "GET", "/api/projects/beta/queue", b"").1["queue"]["entries"][0]["state"], "running", "beta's accept is running its tests");

    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    assert_eq!(started(&s), Vec::<String>::new(), "the machine's one slot is held by beta's accept");
    s.accept_outcomes.lock().unwrap().push(Ok(()));
}

#[test]
fn an_answer_with_control_characters_is_refused_before_anything_is_resumed_or_queued() {
    let s = setup("qra-bad-answer", |_| {});
    stopped_run(&s, "run-old", "failed");
    let bad = json!({"answer": "use \u{1b}[31mv2"});
    let (status, _) = send(&s, "POST", "/api/projects/alpha/runs/run-old/resume", bad.to_string().as_bytes());
    assert_eq!(status, 400);
    let (status, _) = send(&s, "POST", "/api/projects/alpha/queue", json!({"action": "resume", "run": "run-old", "answer": "use \u{7}v2"}).to_string().as_bytes());
    assert_eq!(status, 400);
    assert_eq!(resumed(&s), Vec::<Resumed>::new());
    assert_eq!(states(&s), Vec::<(String, String, String)>::new());
    let (status, _) = send(&s, "POST", "/api/projects/alpha/runs/run-old/resume", json!({"answer": "line one\n\tline two"}).to_string().as_bytes());
    assert_eq!(status, 202, "new lines and tabs are fine");
}

/// Queues an Accept of a needs-review run and then COM-01, lets the accept's tests fail, and checks the
/// queue paused with COM-01 never started (`stop`), or COM-01 started (not `stop`).
fn a_failed_queued_accept(label: &str, stop: bool) {
    let s = setup(label, |settings| settings.queue_stop_on_failure = stop);
    worktree_run(&s, "run-nr", "COM-00", "needs_review");
    add(&s, json!({"action": "accept", "run": "run-nr"}));
    add(&s, json!({"features": ["COM-01"]}));
    round(&s);
    assert_eq!(states(&s), triples(&[("accept", "run-nr", "running"), ("start", "COM-01", "next")]), "stop {stop}");

    finish_accept(&s, "run-nr", Err("3 tests failed".to_string()));
    if stop {
        assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "next")]));
        let paused = queue(&s)["paused"].as_str().unwrap().to_string();
        assert!(paused.starts_with("the run run-nr of Accept run-nr failed: its accept failed: ") && paused.contains("3 tests failed"), "{paused}");
        assert_eq!(started(&s), Vec::<String>::new(), "nothing starts while the queue is paused");
    } else {
        assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "running")]), "the next one starts");
        assert_eq!(queue(&s)["paused"], Value::Null);
        assert_eq!(started(&s), ["COM-01"]);
    }
}

#[test]
fn a_queued_accept_whose_tests_fail_is_marked_failed_and_pauses_the_queue_only_when_stop_on_failure_is_on() {
    a_failed_queued_accept("qra-accept-fail-stop", true);
    a_failed_queued_accept("qra-accept-fail-go", false);
}

#[test]
fn a_failed_queued_accept_pauses_the_queue_before_anything_starts_fifty_times_in_a_row() {
    for _ in 0..50 {
        a_failed_queued_accept("qra-accept-fail-50", true);
    }
}

/// The run whose accept a queue round ends, once that accept is running, just before the round counts
/// the slots for an entry; taken when ended.
type EndInRound = Arc<Mutex<Option<String>>>;

/// A provider change that has queue rounds end the accept [`EndInRound`] names with "3 tests failed"
/// partway through, and wait until it has let go of its run.
fn ending_accepts_mid_round(end_in_round: &EndInRound) -> impl FnOnce(StoreProjects, AcceptOutcomes) -> StoreProjects {
    ending_accepts_mid_round_with(end_in_round, Err("3 tests failed".to_string()))
}

/// [`ending_accepts_mid_round`], the accept's tests ending with `outcome`.
fn ending_accepts_mid_round_with(end_in_round: &EndInRound, outcome: Result<(), String>) -> impl FnOnce(StoreProjects, AcceptOutcomes) -> StoreProjects {
    let end = end_in_round.clone();
    move |projects, outcomes| {
        projects.with_before_queue_slot_count(Arc::new(move |projects: &StoreProjects| {
            let mut end = end.lock().unwrap();
            let Some(run) = end.clone().filter(|run| projects.is_being_accepted(run)) else { return };
            *end = None;
            outcomes.lock().unwrap().push(outcome.clone());
            let until = Instant::now() + Duration::from_secs(5);
            while projects.is_being_accepted(&run) {
                assert!(Instant::now() < until, "the accept of {run} did not end");
                std::thread::sleep(Duration::from_millis(5));
            }
        }))
    }
}

#[test]
fn an_accept_this_server_is_running_shows_running_though_its_run_still_reads_as_needing_review() {
    let s = setup("qra-accept-view", |_| {});
    worktree_run(&s, "run-nr", "COM-00", "needs_review");
    add(&s, json!({"action": "accept", "run": "run-nr"}));
    round(&s);
    let runs = RunStore::new(&s.roots.state);
    let until = Instant::now() + Duration::from_secs(5);
    while !runs.events("run-nr").unwrap().iter().any(|e| e.kind == "run.accept_started") {
        assert!(Instant::now() < until, "the accept of run-nr did not start its tests");
        std::thread::sleep(Duration::from_millis(5));
    }
    // As just after the accept starts, before its thread records it: the run still reads as needing review.
    end_run(&s, "run-nr", "needs_review");
    assert!(s.projects.is_being_accepted("run-nr"));

    assert_eq!(states(&s), triples(&[("accept", "run-nr", "running")]), "an accept whose tests run here is running, not waiting for review");
    s.accept_outcomes.lock().unwrap().push(Err("3 tests failed".to_string()));
}

#[test]
fn under_a_machine_run_limit_an_accept_read_as_going_keeps_the_machine_slot_for_the_whole_round_however_it_ends() {
    for passes in [false, true] {
        let end_in_round = EndInRound::default();
        let outcome = if passes { Ok(()) } else { Err("3 tests failed".to_string()) };
        let label = if passes { "qra-accept-machine-pass" } else { "qra-accept-machine-fail" };
        // No project limit: only the machine's one slot holds COM-01 back.
        let s = setup_with(label, |settings| {
                settings.max_active_runs = None;
                settings.queue_stop_on_failure = true;
            },
            ending_accepts_mid_round_with(&end_in_round, outcome));
        std::fs::write(s.roots.config_file(), "remote_actions = true\n[runs]\nmax_active = 1\n").unwrap();
        worktree_run(&s, "run-nr", "COM-00", "needs_review");
        add(&s, json!({"action": "accept", "run": "run-nr"}));
        add(&s, json!({"features": ["COM-01"]}));
        round(&s);
        assert_eq!(started(&s), Vec::<String>::new(), "passes {passes}: the accept's tests take the machine's one slot");

        *end_in_round.lock().unwrap() = Some("run-nr".to_string());
        round(&s);
        assert_eq!(*end_in_round.lock().unwrap(), None, "passes {passes}: the accept ended partway through the round");
        assert_eq!(started(&s), Vec::<String>::new(), "passes {passes}: the accept read as going holds the machine slot for that whole round");

        round(&s);
        if passes {
            assert_eq!(states(&s), triples(&[("start", "COM-01", "running")]), "the accepted run is done and COM-01 starts next round");
        } else {
            assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "next")]));
            assert!(queue(&s)["paused"].as_str().is_some_and(|why| why.contains("3 tests failed")), "{}", queue(&s)["paused"]);
        }
        assert_eq!(started(&s), if passes { vec!["COM-01".to_string()] } else { Vec::new() }, "passes {passes}");
    }
}

#[test]
fn an_accept_started_this_round_keeps_its_slot_for_the_rest_of_the_round_though_it_fails_partway_through() {
    for stop in [true, false] {
        let end_in_round = EndInRound::default();
        let s = setup_with(if stop { "qra-accept-new-stop" } else { "qra-accept-new-go" }, |settings| settings.queue_stop_on_failure = stop, ending_accepts_mid_round(&end_in_round));
        worktree_run(&s, "run-nr", "COM-00", "needs_review");
        add(&s, json!({"action": "accept", "run": "run-nr"}));
        add(&s, json!({"features": ["COM-01"]}));
        *end_in_round.lock().unwrap() = Some("run-nr".to_string());
        round(&s);
        assert_eq!(*end_in_round.lock().unwrap(), None, "stop {stop}: the accept ended in the round that started it");
        assert_eq!(started(&s), Vec::<String>::new(), "stop {stop}: the accept started this round holds its slot for the rest of it");

        round(&s);
        if stop {
            assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "next")]));
            assert!(queue(&s)["paused"].as_str().is_some_and(|why| why.contains("3 tests failed")), "{}", queue(&s)["paused"]);
            assert_eq!(started(&s), Vec::<String>::new(), "COM-01 never starts");
        } else {
            assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "running")]), "the next round starts COM-01");
            assert_eq!(started(&s), ["COM-01"]);
        }
    }
}

#[test]
fn an_accept_read_as_going_keeps_its_slot_for_the_whole_round_though_it_fails_partway_through() {
    for stop in [true, false] {
        let end_in_round = EndInRound::default();
        let s = setup_with(if stop { "qra-accept-mid-stop" } else { "qra-accept-mid-go" }, |settings| settings.queue_stop_on_failure = stop, ending_accepts_mid_round(&end_in_round));
        worktree_run(&s, "run-nr", "COM-00", "needs_review");
        add(&s, json!({"action": "accept", "run": "run-nr"}));
        add(&s, json!({"features": ["COM-01"]}));
        round(&s);
        assert_eq!(states(&s), triples(&[("accept", "run-nr", "running"), ("start", "COM-01", "next")]), "stop {stop}");

        *end_in_round.lock().unwrap() = Some("run-nr".to_string());
        round(&s);
        assert_eq!(started(&s), Vec::<String>::new(), "stop {stop}: the accept read as going holds its slot for that whole round");

        round(&s);
        if stop {
            assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "next")]));
            assert!(queue(&s)["paused"].as_str().is_some_and(|why| why.contains("3 tests failed")), "{}", queue(&s)["paused"]);
            assert_eq!(started(&s), Vec::<String>::new(), "COM-01 never starts");
        } else {
            assert_eq!(states(&s), triples(&[("accept", "run-nr", "failed"), ("start", "COM-01", "running")]), "the next round starts COM-01");
            assert_eq!(started(&s), ["COM-01"]);
        }
    }
}
