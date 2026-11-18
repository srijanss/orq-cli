//! The web's Accept does what the popup's Accept does: a failed or needs_review run whose worktree holds clean,
//! committed work has its tests run there and becomes review ready when they pass. The checks before the tests
//! answer at once; the tests run after the reply, and a second Accept while they do is refused.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{handle_with, handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    Setup { scan: base.join("code"), roots }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["commit", "-q", "--allow-empty", "-m", "start"]);
    path
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

/// A run of `repo` whose worktree is a real checkout of its own branch, one commit ahead of main.
fn add_run(s: &Setup, repo: &Path, run: &str, kinds: &[&str]) {
    let project = project_id(repo);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let worktree = repo.join(".claude").join("worktrees").join(run);
    let branch = format!("orq/{run}");
    git(repo, &["worktree", "add", "-q", "-b", &branch, worktree.to_str().unwrap()]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", &format!("work of {run}")]);
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.clone(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: base, branch, worktree, repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, kind) in kinds.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.clone(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: None, attempt: 1, occurred_at: format!("2026-11-01T10:00:0{n}Z"), payload: json!({}) };
        store.append_event(&event).unwrap();
    }
}

type Tests = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

fn post(s: &Setup, provider: &StoreProjects, run: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}/accept");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, provider);
    (r.status, r.body)
}

fn kinds(s: &Setup, run: &str) -> Vec<String> {
    RunStore::new(&s.roots.state).events(run).unwrap().into_iter().map(|e| e.kind).collect()
}

/// The run's events once they number `count`, or as they are after a few seconds.
fn kinds_once(s: &Setup, run: &str, count: usize) -> Vec<String> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let now = kinds(s, run);
        if now.len() >= count || Instant::now() > until {
            return now;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_failed_run_with_clean_committed_work_is_tested_after_the_reply_and_refusals_answer_at_once() {
    let s = setup("run-accept");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    add_run(&s, &alpha, "r-dirty", &["run.started", "run.failed"]);
    std::fs::write(alpha.join(".claude/worktrees/r-dirty/left.txt"), "x").unwrap();
    add_run(&s, &alpha, "r-run", &["run.started"]);
    add_run(&s, &beta, "r-beta", &["run.started", "run.failed"]);
    let tested = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = tested.clone();
    let tests: Tests = Arc::new(move |worktree: &Path| {
        seen.lock().unwrap().push(worktree.file_name().unwrap().to_string_lossy().into_owned());
        Ok(())
    });
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_accept_tests(tests);

    assert_eq!(post(&s, &provider, "r-fail"), (202, json!({"run": "r-fail"})));
    assert_eq!(kinds_once(&s, "r-fail", 4), ["run.started", "run.failed", "run.accept_started", "run.completed"], "its tests passed, so it is review ready");
    assert_eq!(post(&s, &provider, "r-dirty"), (409, json!({"error": "r-dirty has uncommitted changes in its worktree; commit or discard them first"})));
    assert_eq!(post(&s, &provider, "r-run"), (409, json!({"error": "r-run is running; only a failed run can be accepted, or one waiting on a review decision"})));
    assert_eq!(post(&s, &provider, "r-beta").0, 404, "a run of another project is not this project's");
    assert_eq!(*tested.lock().unwrap(), ["r-fail"], "only an acceptable run is tested");
}

#[test]
fn a_run_whose_tests_are_running_is_not_accepted_twice_and_failing_tests_leave_it_failed() {
    let s = setup("run-accept-twice");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    let (started, on_start) = mpsc::channel::<()>();
    let (finish, on_finish) = mpsc::channel::<()>();
    let gate = Mutex::new((started, on_finish));
    let tests: Tests = Arc::new(move |_: &Path| {
        let gate = gate.lock().unwrap();
        gate.0.send(()).unwrap();
        gate.1.recv().unwrap();
        Err("1 test failed".into())
    });
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_accept_tests(tests);

    assert_eq!(post(&s, &provider, "r-fail").0, 202);
    on_start.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(post(&s, &provider, "r-fail"), (409, json!({"error": "r-fail is already being accepted"})));
    finish.send(()).unwrap();
    assert_eq!(kinds_once(&s, "r-fail", 4), ["run.started", "run.failed", "run.accept_started", "run.accept_failed"], "failing tests are recorded");
}

fn act(s: &Setup, provider: &StoreProjects, run: &str, action: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}/{action}");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), provider);
    (r.status, r.body)
}

#[test]
fn while_its_tests_run_a_run_being_accepted_cannot_be_resumed_stopped_paused_switched_or_discarded() {
    let s = setup("run-accept-excludes");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    let (started, on_start) = mpsc::channel::<()>();
    let (finish, on_finish) = mpsc::channel::<()>();
    let gate = Mutex::new((started, on_finish));
    let tests: Tests = Arc::new(move |_: &Path| {
        let gate = gate.lock().unwrap();
        gate.0.send(()).unwrap();
        gate.1.recv().unwrap();
        Ok(())
    });
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_accept_tests(tests);

    assert_eq!(post(&s, &provider, "r-fail").0, 202);
    on_start.recv_timeout(Duration::from_secs(5)).unwrap();
    let busy = json!({"error": "r-fail: an accept is running; try again when its tests have finished"});
    for (action, body) in [("resume", ""), ("pause", ""), ("stop", ""), ("discard", ""), ("model", r#"{"role":"implementer","profile":"fast"}"#)] {
        assert_eq!(act(&s, &provider, "r-fail", action, body), (409, busy.clone()), "{action}");
    }
    assert!(alpha.join(".claude/worktrees/r-fail").exists(), "nothing was discarded");
    finish.send(()).unwrap();
    assert_eq!(kinds_once(&s, "r-fail", 4), ["run.started", "run.failed", "run.accept_started", "run.completed"]);
}

fn run_json(s: &Setup, provider: &StoreProjects, run: &str) -> Value {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{run}");
    let request = Request { method: "GET", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, provider);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body["run"].clone()
}

#[test]
fn while_accepting_the_run_says_so_with_since_when_and_a_failure_shows_why_with_the_output() {
    let s = setup("run-accept-shown");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    let (started, on_start) = mpsc::channel::<()>();
    let (finish, on_finish) = mpsc::channel::<()>();
    let gate = Mutex::new((started, on_finish));
    let tests: Tests = Arc::new(move |_: &Path| {
        let gate = gate.lock().unwrap();
        gate.0.send(()).unwrap();
        gate.1.recv().unwrap();
        Err("`cargo test` failed: test result: FAILED. 1 failed".into())
    });
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_accept_tests(tests);

    assert_eq!(post(&s, &provider, "r-fail").0, 202);
    on_start.recv_timeout(Duration::from_secs(5)).unwrap();
    let during = run_json(&s, &provider, "r-fail");
    assert_eq!(during["state"], "accepting");
    assert!(during["accepting_since"].is_string(), "{during}");
    let (status, body) = act(&s, &provider, "r-fail", "resume", "");
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("an accept is running"), "{body}");

    finish.send(()).unwrap();
    kinds_once(&s, "r-fail", 4);
    let after = run_json(&s, &provider, "r-fail");
    assert_eq!(after["state"], "failed");
    assert!(after["accept_failure"]["reason"].as_str().unwrap().contains("`cargo test` failed"), "{after}");
    assert!(after["accept_failure"]["output"].as_str().unwrap().contains("1 failed"), "{after}");
    assert!(after["accept_failure"]["at"].is_string(), "{after}");
}

#[test]
fn a_server_started_after_one_stopped_mid_accept_records_the_accept_as_failed() {
    let s = setup("run-accept-restart");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-stuck", &["run.started", "run.failed", "run.accept_started"]);
    add_run(&s, &alpha, "r-other", &["run.started", "run.failed"]);

    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());

    assert_eq!(kinds(&s, "r-stuck").last().map(String::as_str), Some("run.accept_failed"));
    assert_eq!(kinds(&s, "r-other"), ["run.started", "run.failed"], "a run that was not accepting is left alone");
    let shown = run_json(&s, &provider, "r-stuck");
    assert_eq!(shown["state"], "failed");
    assert_eq!(shown["accept_failure"]["reason"], "the server stopped while the tests were running; accept again");
}

#[test]
fn a_passing_accept_shows_the_run_review_ready_with_its_delivery() {
    let s = setup("run-accept-passed");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    let tests: Tests = Arc::new(|_: &Path| Ok(()));
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_accept_tests(tests);

    assert_eq!(post(&s, &provider, "r-fail").0, 202);
    kinds_once(&s, "r-fail", 4);
    let shown = run_json(&s, &provider, "r-fail");
    assert_eq!(shown["state"], "completed");
    assert_eq!(shown["accepted"], true, "{shown}");
    assert!(shown["accept_failure"].is_null(), "{shown}");
}
