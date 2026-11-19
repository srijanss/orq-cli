//! In a project whose runs end in an MR, the MR is merged on the forge and the project's local target
//! branch is left behind. Before the queue starts its next entry it pulls the target, fast-forward
//! only, so the run starts from the merged code; what cannot be fast-forwarded pauses the queue with
//! the reason. Each round is driven here with a fake launcher, against a temporary project cloned from
//! a bare origin.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle};
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Starts a run the way `orq run` records one: from the local target branch, as it is now.
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
    /// The project's main checkout, a clone of `origin`.
    repo: PathBuf,
    /// The bare repository the project pushes to and the MR merges into.
    origin: PathBuf,
    /// Another clone of `origin`, standing for the forge merging MRs.
    forge: PathBuf,
}

/// A project `alpha`, cloned from a bare origin, with two independent approved features COM-01 and
/// COM-02 queued in that order, delivered by `delivery`, and `settings` changed further by `change`.
fn setup(label: &str, delivery: Delivery, change: impl FnOnce(&mut ProjectSettings)) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), started: Mutex::default() });
    let origin = base.join("origin.git");
    git(&base, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    let scan = base.join("code");
    std::fs::create_dir_all(&scan).unwrap();
    git(&scan, &["clone", "-q", origin.to_str().unwrap(), "alpha"]);
    let repo = std::fs::canonicalize(scan.join("alpha")).unwrap();
    git(&repo, &["checkout", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "start\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".tdd-*.json\n.claude/\n").unwrap();
    git(&repo, &["add", "README", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let forge = base.join("forge");
    git(&base, &["clone", "-q", origin.to_str().unwrap(), forge.to_str().unwrap()]);
    let ledger = json!([{"featureName": "COM-01", "dependsOn": [], "status": "pending"}, {"featureName": "COM-02", "dependsOn": [], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let mut settings = ProjectSettings { delivery, max_active_runs: Some(1), ..ProjectSettings::default() };
    change(&mut settings);
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup { scan, roots, launcher, repo, origin, forge };
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
            occurred_at: "2026-11-19T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

/// A run of `feature` in the project at `repo`, started from the local `main` and still running.
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
        base_sha: git(repo, &["rev-parse", "refs/heads/main"]),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

/// Runs git in `dir` and gives back what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The forge merges an MR: origin's `main` gets a new commit. Its sha.
fn merge_on_origin(s: &Setup, message: &str) -> String {
    git(&s.forge, &["pull", "-q", "--ff-only", "origin", "main"]);
    std::fs::write(s.forge.join(format!("{message}.txt")), format!("{message}\n")).unwrap();
    git(&s.forge, &["add", "."]);
    git(&s.forge, &["commit", "-q", "-m", message]);
    git(&s.forge, &["push", "-q", "origin", "main"]);
    git(&s.forge, &["rev-parse", "HEAD"])
}

fn base_sha(s: &Setup, run: &str) -> String {
    RunStore::new(&s.roots.state).load(run).unwrap().base_sha
}

#[test]
fn an_mr_merged_on_origin_is_pulled_before_the_next_queued_start() {
    let s = setup("pull-before-start", Delivery::Review, |_| {});
    let merged = merge_on_origin(&s, "merged-mr");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), merged, "the local target was fast-forwarded to origin's");
    assert_eq!(base_sha(&s, "run-1-COM-01"), merged, "the run starts from the merged code");
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), merged, "the checkout on main moved with it");
    assert!(s.repo.join("merged-mr.txt").exists(), "the merged file is in the checkout");
    assert_eq!(queue(&s)["paused"], Value::Null);
    let _ = &s.origin;
}

#[test]
fn a_changed_tracked_file_pauses_the_queue_naming_it_until_resumed_after_cleaning() {
    let s = setup("pull-dirty", Delivery::Review, |_| {});
    let before = git(&s.repo, &["rev-parse", "HEAD"]);
    std::fs::write(s.repo.join("README"), "edited by hand\n").unwrap();
    let merged = merge_on_origin(&s, "merged-mr");
    round(&s);
    assert!(started(&s).is_empty(), "nothing starts on old code");
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), before, "nothing moved");
    let paused = queue(&s)["paused"].as_str().unwrap_or_default().to_string();
    assert!(paused.contains("README"), "the pause names the file: {paused}");
    round(&s);
    assert!(started(&s).is_empty(), "still paused");

    git(&s.repo, &["checkout", "--", "README"]);
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue/resume", b"");
    assert_eq!(status, 200, "{body}");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(base_sha(&s, "run-1-COM-01"), merged);
}

#[test]
fn a_target_diverged_from_origin_pauses_the_queue_with_both_counts() {
    let s = setup("pull-diverged", Delivery::Review, |_| {});
    std::fs::write(s.repo.join("local.txt"), "local\n").unwrap();
    git(&s.repo, &["add", "local.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "local only"]);
    let local = git(&s.repo, &["rev-parse", "HEAD"]);
    merge_on_origin(&s, "one");
    merge_on_origin(&s, "two");
    round(&s);
    assert!(started(&s).is_empty());
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), local, "nothing moved");
    let paused = queue(&s)["paused"].as_str().unwrap_or_default().to_string();
    assert!(paused.contains("main has 1 commit not on origin and origin has 2 not here"), "{paused}");
}

/// Points the project's `origin` at a forge that cannot be reached, with a token in its URL.
fn break_origin(s: &Setup) {
    git(&s.repo, &["remote", "set-url", "origin", "https://orq:s3cr3t-token@127.0.0.1:9/alpha.git"]);
}

fn mend_origin(s: &Setup) {
    git(&s.repo, &["remote", "set-url", "origin", s.origin.to_str().unwrap()]);
}

#[test]
fn a_fetch_failing_twice_is_retried_without_pausing_and_the_third_success_starts() {
    let s = setup("pull-fetch-retry", Delivery::Review, |_| {});
    let merged = merge_on_origin(&s, "merged-mr");
    break_origin(&s);
    round(&s);
    round(&s);
    assert!(started(&s).is_empty(), "nothing starts while the target cannot be fetched");
    assert_eq!(queue(&s)["paused"], Value::Null, "two failures in a row do not pause");
    mend_origin(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(base_sha(&s, "run-1-COM-01"), merged);
}

#[test]
fn a_fetch_failing_three_rounds_in_a_row_pauses_the_queue_with_the_message_redacted() {
    let s = setup("pull-fetch-fails", Delivery::Review, |_| {});
    break_origin(&s);
    round(&s);
    round(&s);
    assert_eq!(queue(&s)["paused"], Value::Null);
    round(&s);
    assert!(started(&s).is_empty());
    let paused = queue(&s)["paused"].as_str().unwrap_or_default().to_string();
    assert!(paused.contains("main could not be fetched from origin"), "{paused}");
    assert!(!paused.contains("s3cr3t-token"), "the token is redacted: {paused}");
}

#[test]
fn a_success_between_fetch_failures_starts_the_count_again() {
    let s = setup("pull-fetch-count", Delivery::Review, |_| {});
    break_origin(&s);
    round(&s);
    round(&s);
    mend_origin(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    // COM-01's run is still going, and holds the one slot: the next rounds pull nothing.
    let store = RunStore::new(&s.roots.state);
    event(&store, "run-1-COM-01", "run.completed", json!({}));
    break_origin(&s);
    round(&s);
    round(&s);
    assert_eq!(queue(&s)["paused"], Value::Null, "the count started again after the success");
}

#[test]
fn a_merge_local_project_is_not_fetched() {
    let s = setup("pull-merge-local", Delivery::MergeLocal, |_| {});
    let before = git(&s.repo, &["rev-parse", "refs/heads/main"]);
    merge_on_origin(&s, "merged-mr");
    break_origin(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "a merge-local start does not wait on the forge");
    assert_eq!(base_sha(&s, "run-1-COM-01"), before);
    assert_eq!(queue(&s)["paused"], Value::Null);
}

#[test]
fn with_pulling_turned_off_the_target_is_not_fetched() {
    let s = setup("pull-off", Delivery::Review, |settings| settings.pull_target_before_runs = false);
    let before = git(&s.repo, &["rev-parse", "refs/heads/main"]);
    merge_on_origin(&s, "merged-mr");
    break_origin(&s);
    round(&s);
    assert_eq!(started(&s), ["COM-01"], "the start does not wait on the forge, as before");
    assert_eq!(base_sha(&s, "run-1-COM-01"), before);
}

fn project_settings(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/settings", b"");
    assert_eq!(status, 200, "{body}");
    body["settings"].clone()
}

#[test]
fn pulling_is_on_by_default_and_saved_off_from_the_settings() {
    let s = setup("pull-setting", Delivery::Review, |_| {});
    let shown = project_settings(&s);
    assert_eq!(shown["pull_target_before_runs"], true, "{shown}");
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": 1, "min_free_disk_gb": null, "revision": shown["revision"]});
    body["pull_target_before_runs"] = json!(false);
    let (status, reply) = send(&s, "POST", "/api/projects/alpha/settings", body.to_string().as_bytes());
    assert_eq!(status, 200, "{reply}");
    assert_eq!(project_settings(&s)["pull_target_before_runs"], false);

    let mut bad = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": 1, "min_free_disk_gb": null, "revision": project_settings(&s)["revision"]});
    bad["pull_target_before_runs"] = json!("no");
    assert_eq!(send(&s, "POST", "/api/projects/alpha/settings", bad.to_string().as_bytes()).0, 400);
    assert_eq!(project_settings(&s)["pull_target_before_runs"], false, "a refused save writes nothing");
}

#[test]
fn a_checkout_on_another_branch_has_the_target_moved_under_it_and_starts() {
    let s = setup("pull-other-branch", Delivery::Review, |_| {});
    git(&s.repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(s.repo.join("README"), "edited on side\n").unwrap();
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    let merged = merge_on_origin(&s, "merged-mr");
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(base_sha(&s, "run-1-COM-01"), merged);
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), head, "HEAD stays where it was");
    assert_eq!(git(&s.repo, &["symbolic-ref", "--short", "HEAD"]), "side");
    assert_eq!(std::fs::read_to_string(s.repo.join("README")).unwrap(), "edited on side\n", "the files are left alone");
    assert!(!s.repo.join("merged-mr.txt").exists());
}

/// What the review refresh saved about `run`'s MR, !12: `state` at the provider.
fn mr(s: &Setup, run: &str, state: ProviderState) {
    let mut lifecycle = ReviewLifecycle::associate("gitlab", "12", "https://gitlab.example/alpha/-/merge_requests/12", &"b".repeat(40));
    lifecycle.state = state;
    lifecycle.observed = true;
    Store::open(&s.roots.database()).unwrap().save_lifecycle(run, &lifecycle).unwrap();
}

fn events_of(s: &Setup, run: &str, kind: &str) -> Vec<Value> {
    RunStore::new(&s.roots.state).events(run).unwrap().into_iter().filter(|event| event.kind == kind).map(|event| event.payload).collect()
}

/// The dashboard card of project alpha.
fn card(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects", b"");
    assert_eq!(status, 200, "{body}");
    body["projects"].as_array().unwrap().iter().find(|project| project["id"] == "alpha").cloned().unwrap()
}

#[test]
fn the_refresh_seeing_the_mr_merged_pulls_the_target_before_any_start_and_records_it() {
    let s = setup("pull-on-merge", Delivery::Review, |_| {});
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    let merged = merge_on_origin(&s, "merged-mr");
    mr(&s, "run-1-COM-01", ProviderState::Merged);

    provider(&s).review_looked_at("run-1-COM-01");
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), merged, "pulled before any round");
    let pulled = events_of(&s, "run-1-COM-01", "target.pulled");
    assert_eq!(pulled.len(), 1, "{pulled:?}");
    assert_eq!(pulled[0]["outcome"], "fast_forwarded");
    assert_eq!(pulled[0]["to"], merged.as_str());
    let audit = orq_core::remote_audit::read(&s.roots, 10).unwrap();
    let entry = audit.iter().find(|entry| entry["action"] == "project.pull").expect("an audit entry");
    assert_eq!(entry["identity"], "orq serve");
    assert_eq!(entry["outcome"], "allowed");
    assert!(card(&s)["queue"].get("target_pull_warning").is_none());

    provider(&s).review_looked_at("run-1-COM-01");
    assert_eq!(events_of(&s, "run-1-COM-01", "target.pulled").len(), 1, "one merge is pulled once");
}

#[test]
fn an_open_mr_or_a_merge_local_project_pulls_nothing_on_a_look() {
    let s = setup("pull-on-open", Delivery::Review, |_| {});
    round(&s);
    let before = git(&s.repo, &["rev-parse", "refs/heads/main"]);
    merge_on_origin(&s, "other");
    mr(&s, "run-1-COM-01", ProviderState::Open);
    provider(&s).review_looked_at("run-1-COM-01");
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(events_of(&s, "run-1-COM-01", "target.pulled").is_empty());
}

#[test]
fn a_merge_that_cannot_be_pulled_warns_on_the_card_and_page_until_a_retry_pulls_it() {
    let s = setup("pull-on-merge-diverged", Delivery::Review, |_| {});
    round(&s);
    std::fs::write(s.repo.join("local.txt"), "local\n").unwrap();
    git(&s.repo, &["add", "local.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "local only"]);
    let merged = merge_on_origin(&s, "merged-mr");
    mr(&s, "run-1-COM-01", ProviderState::Merged);
    provider(&s).review_looked_at("run-1-COM-01");

    let pulled = events_of(&s, "run-1-COM-01", "target.pulled");
    assert_eq!(pulled[0]["outcome"], "diverged");
    let warning = "main not updated after !12 merged: main has 1 commit not on origin and origin has 1 not here";
    assert_eq!(card(&s)["queue"]["target_pull_warning"], warning);
    assert_eq!(queue(&s)["target_pull_warning"], warning);

    // Still diverged: the retry says so and the warning stays.
    let (status, body) = send(&s, "POST", "/api/projects/alpha/pull", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outcome"], "diverged");
    assert_eq!(queue(&s)["target_pull_warning"], warning);

    git(&s.repo, &["reset", "-q", "--hard", "HEAD~1"]);
    let (status, body) = send(&s, "POST", "/api/projects/alpha/pull", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outcome"], "fast_forwarded");
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), merged);
    assert!(queue(&s).get("target_pull_warning").is_none());
    assert!(card(&s)["queue"].get("target_pull_warning").is_none());
    let audit = orq_core::remote_audit::read(&s.roots, 10).unwrap();
    assert_eq!(audit[0]["action"], "project.pull");
    assert_eq!(audit[0]["identity"], "web");
}

#[test]
fn a_retry_needs_remote_actions_and_a_known_project() {
    let s = setup("pull-retry-off", Delivery::Review, |_| {});
    assert_eq!(send(&s, "POST", "/api/projects/nobody/pull", b"").0, 404);
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(send(&s, "POST", "/api/projects/alpha/pull", b"").0, 404);
    let audit = orq_core::remote_audit::read(&s.roots, 1).unwrap();
    assert_eq!(audit[0]["action"], "project.pull");
    assert_eq!(audit[0]["outcome"], "refused");
}

#[test]
fn a_target_only_ahead_of_origin_starts_without_pulling() {
    let s = setup("pull-ahead", Delivery::Review, |_| {});
    std::fs::write(s.repo.join("local.txt"), "local\n").unwrap();
    git(&s.repo, &["add", "local.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "local only"]);
    let local = git(&s.repo, &["rev-parse", "HEAD"]);
    round(&s);
    assert_eq!(started(&s), ["COM-01"]);
    assert_eq!(base_sha(&s, "run-1-COM-01"), local);
    assert_eq!(queue(&s)["paused"], Value::Null);
}
