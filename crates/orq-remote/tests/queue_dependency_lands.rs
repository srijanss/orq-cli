//! A queued feature's dependency is met only once its work has landed where the next run starts from:
//! a run's session marks its feature completed at the end of its implement stage, well before its gate,
//! push and MR, so the ledger alone would start the dependent from a target that lacks that work. Each
//! dependency with a run is judged by its newest run (followed through its resumes); one with no run by
//! the ledger alone, as before. Each round is driven here with a fake launcher, against a temporary
//! project cloned from a bare origin.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle};
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
        let run = format!("run-q{}-{feature}", started.len());
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
    /// Another clone of the project's origin, standing for the forge merging MRs.
    forge: PathBuf,
}

/// A project `alpha`, cloned from a bare origin and delivered by `delivery` with no run limit, whose
/// ledger holds DEP-A completed and DEP-B, which depends on it, approved; DEP-B is queued.
fn setup(label: &str, delivery: Delivery) -> Setup {
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
    let ledger = json!([{"featureName": "DEP-A", "dependsOn": [], "status": "completed"}, {"featureName": "DEP-B", "dependsOn": ["DEP-A"], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let settings = ProjectSettings { delivery, max_active_runs: None, ..ProjectSettings::default() };
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup { scan, roots, launcher, repo, forge };
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue", json!({"features": ["DEP-B"]}).to_string().as_bytes());
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

/// DEP-B's entry as the queue page reads it.
fn entry_b(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"]["entries"].as_array().unwrap().iter().find(|entry| entry["feature"] == "DEP-B").cloned().unwrap()
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

/// A run `run` of DEP-A, going.
fn run_a(s: &Setup, run: &str) {
    seed_run(&s.roots.state, &s.repo, run, "DEP-A");
}

/// Ends `run` with `ending` (`merged`, `review_ready`, `failed`, ...).
fn end_run(s: &Setup, run: &str, ending: &str) {
    event(&RunStore::new(&s.roots.state), run, "run.completed", json!({"ending": ending}));
}

/// DEP-A's run `run` commits its work on its branch, pushes it and ends delivered for review, with an
/// MR in `state` on that head. The head's sha.
fn deliver_a(s: &Setup, run: &str, state: ProviderState) -> String {
    let branch = format!("orq/{run}");
    git(&s.repo, &["checkout", "-q", "-b", &branch, "main"]);
    std::fs::write(s.repo.join("dep-a.txt"), "dep a\n").unwrap();
    git(&s.repo, &["add", "dep-a.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "dep a"]);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    git(&s.repo, &["checkout", "-q", "main"]);
    git(&s.repo, &["push", "-q", "origin", &branch]);
    end_run(s, run, "review_ready");
    mr(s, run, &head, state);
    head
}

/// Saves the newest MR of `run`, on `head`, in `state`.
fn mr(s: &Setup, run: &str, head: &str, state: ProviderState) {
    let mut lifecycle = ReviewLifecycle::associate("gitlab", "12", "https://gitlab.example/alpha/-/merge_requests/12", head);
    lifecycle.state = state;
    lifecycle.observed = true;
    Store::open(&s.roots.database()).unwrap().save_lifecycle(run, &lifecycle).unwrap();
}

/// The forge merges `run`'s branch into origin's `main`, with a merge commit.
fn merge_on_origin(s: &Setup, run: &str) {
    git(&s.forge, &["fetch", "-q", "origin"]);
    git(&s.forge, &["merge", "-q", "--no-ff", "-m", "merge", &format!("origin/orq/{run}")]);
    git(&s.forge, &["push", "-q", "origin", "main"]);
}

/// Runs git in `dir` and gives back what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_dependency_completed_in_the_ledger_whose_run_is_still_going_holds_its_dependent() {
    let s = setup("dep-lands-running", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    round(&s);
    assert!(started(&s).is_empty(), "DEP-B does not start while DEP-A's run is in its gate");
    let entry = entry_b(&s);
    assert_eq!(entry["state"], "waiting");
    assert_eq!(entry["reason"], "waiting for DEP-A (running)");
}

#[test]
fn a_failed_dependency_holds_its_dependent_until_the_run_it_was_resumed_as_merges() {
    let s = setup("dep-lands-failed", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    end_run(&s, "run-1-DEP-A", "failed");
    round(&s);
    assert!(started(&s).is_empty(), "DEP-B does not start after DEP-A's run failed");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (failed)");

    run_a(&s, "run-2-DEP-A");
    event(&RunStore::new(&s.roots.state), "run-1-DEP-A", "run.resumed", json!({"resumed_as": "run-2-DEP-A"}));
    round(&s);
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (running)", "the resume is followed");

    end_run(&s, "run-2-DEP-A", "merged");
    round(&s);
    assert_eq!(started(&s), ["DEP-B"], "a merge-local run that ended merged has landed");
}

#[test]
fn a_dependency_delivered_for_review_lands_once_its_mr_merged_and_the_target_was_pulled() {
    let s = setup("dep-lands-review", Delivery::Review);
    run_a(&s, "run-1-DEP-A");
    let head = deliver_a(&s, "run-1-DEP-A", ProviderState::Open);
    round(&s);
    assert!(started(&s).is_empty(), "DEP-B does not start while DEP-A's MR is open");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (in review)");

    merge_on_origin(&s, "run-1-DEP-A");
    mr(&s, "run-1-DEP-A", &head, ProviderState::Merged);
    round(&s);
    assert!(started(&s).is_empty(), "DEP-B does not start from a target that lacks DEP-A's merge");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (merged, not pulled)");

    git(&s.repo, &["pull", "-q", "--ff-only", "origin", "main"]);
    round(&s);
    assert_eq!(started(&s), ["DEP-B"]);
    let base = RunStore::new(&s.roots.state).load("run-q1-DEP-B").unwrap().base_sha;
    git(&s.repo, &["merge-base", "--is-ancestor", &head, &base]);
}

#[test]
fn a_squash_merged_dependency_lands_once_the_refresh_pulled_its_merge() {
    let s = setup("dep-lands-squash", Delivery::Review);
    run_a(&s, "run-1-DEP-A");
    let head = deliver_a(&s, "run-1-DEP-A", ProviderState::Open);
    git(&s.forge, &["fetch", "-q", "origin"]);
    git(&s.forge, &["merge", "-q", "--squash", "origin/orq/run-1-DEP-A"]);
    git(&s.forge, &["commit", "-q", "-m", "dep a (squashed)"]);
    git(&s.forge, &["push", "-q", "origin", "main"]);
    mr(&s, "run-1-DEP-A", &head, ProviderState::Merged);
    round(&s);
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (merged, not pulled)");

    provider(&s).review_looked_at("run-1-DEP-A");
    round(&s);
    assert_eq!(started(&s), ["DEP-B"], "the squashed head is never on the target, but the pull after the merge brought it level");
}

#[test]
fn a_dependency_whose_mr_closed_unmerged_holds_its_dependent_with_that_reason() {
    let s = setup("dep-lands-closed", Delivery::Review);
    run_a(&s, "run-1-DEP-A");
    deliver_a(&s, "run-1-DEP-A", ProviderState::Closed);
    round(&s);
    assert!(started(&s).is_empty());
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (MR closed without merging)");
}

#[test]
fn a_dependency_completed_in_the_ledger_that_never_ran_is_met_as_before() {
    let s = setup("dep-lands-no-run", Delivery::Review);
    round(&s);
    assert_eq!(started(&s), ["DEP-B"]);
}

/// A run that ended delivered for review with no MR recorded for it yet has given no sign that its work
/// reached the target; a review kept here and merged into the target locally has.
#[test]
fn a_dependency_delivered_for_review_with_no_mr_recorded_holds_its_dependent_until_it_is_merged_locally() {
    let s = setup("dep-lands-no-mr", Delivery::Review);
    run_a(&s, "run-1-DEP-A");
    end_run(&s, "run-1-DEP-A", "review_ready");
    round(&s);
    assert!(started(&s).is_empty(), "no MR is no evidence that DEP-A landed");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (in review)");

    let head = git(&s.repo, &["rev-parse", "main"]);
    Store::open(&s.roots.database()).unwrap().save_lifecycle("run-1-DEP-A", &ReviewLifecycle::merged_locally(&head, &head)).unwrap();
    round(&s);
    assert_eq!(started(&s), ["DEP-B"], "merged into the target here");
}

/// A run that cannot be read gives no sign its work landed: the run a failed one was resumed as, since
/// discarded, holds the dependent with that reason, and so do events that cannot be read.
#[test]
fn a_dependency_whose_run_cannot_be_read_holds_its_dependent() {
    let s = setup("dep-lands-unreadable", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    end_run(&s, "run-1-DEP-A", "failed");
    event(&RunStore::new(&s.roots.state), "run-1-DEP-A", "run.resumed", json!({"resumed_as": "run-2-DEP-A"}));
    round(&s);
    assert!(started(&s).is_empty(), "the run it was resumed as is gone");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (discarded)");

    let s = setup("dep-lands-corrupt", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    end_run(&s, "run-1-DEP-A", "merged");
    let events = s.roots.state.join("runs").join("run-1-DEP-A").join("events.jsonl");
    assert!(events.is_file(), "{}", events.display());
    std::fs::write(&events, "not json\n").unwrap();
    round(&s);
    assert!(started(&s).is_empty(), "events that cannot be read are no sign it landed");
    assert_eq!(entry_b(&s)["reason"], "waiting for DEP-A (its run cannot be read)");
}

/// A manual Start is never refused for an unlanded dependency: someone is there to judge it, so the
/// Start popup is told the same reason the queue page shows, as a warning.
#[test]
fn a_manual_start_of_a_feature_whose_dependency_has_not_landed_starts_with_that_reason_as_a_warning() {
    let s = setup("dep-lands-manual", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    let (status, body) = send(&s, "POST", "/api/projects/alpha/features/DEP-B/runs", b"");
    assert_eq!(status, 202, "{body}");
    assert_eq!(body, json!({"run": "run-q1-DEP-B", "warning": "waiting for DEP-A (running)"}));

    let s = setup("dep-lands-manual-landed", Delivery::MergeLocal);
    run_a(&s, "run-1-DEP-A");
    end_run(&s, "run-1-DEP-A", "merged");
    assert_eq!(send(&s, "POST", "/api/projects/alpha/features/DEP-B/runs", b""), (202, json!({"run": "run-q1-DEP-B"})), "no warning once it landed");
}
