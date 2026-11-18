//! orq serve repairs what a run could not record: the PR/MR it created, and the report of its head, built from the
//! gate and CI results the run's own events hold.

mod web_deps;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use orq_core::storage::StorageRoots;
use orq_core::store::{ReviewRecord, Store};
use orq_orchestrator::{CiOutcome, Event, RunRecord, RunStore};
use orq_remote::projects::{ReportRepair, StoreProjects};
use serde_json::{json, Value};

const HEAD: &str = "49485ef0000000000000000000000000000000aa";
const NO_INDEX: &str = "cannot open the review index: orq.db has schema version 9, newer than this Orq understands (8)";

struct Setup {
    roots: StorageRoots,
    scan: PathBuf,
    runs: RunStore,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    drop(Store::open(&roots.database()).unwrap());
    let runs = RunStore::new(&roots.state);
    Setup { roots, scan: base.join("code"), runs }
}

fn event(s: &Setup, run_id: &str, kind: &str, stage: Option<&str>, payload: Value) {
    let n = s.runs.events(run_id).unwrap().len();
    s.runs
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run_id}-{n}"),
            run_id: run_id.into(),
            project_id: "alpha-1".into(),
            feature_id: "admin-alert-dispatch".into(),
            kind: kind.into(),
            stage: stage.map(str::to_string),
            attempt: 1,
            occurred_at: "2026-11-18T06:55:00Z".into(),
            payload,
        })
        .unwrap();
}

/// A run that passed its gate and CI at `HEAD`, created MR 976, and ended review_ready.
fn ended_run(s: &Setup, run_id: &str) {
    s.runs
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: run_id.into(),
            project_id: "alpha-1".into(),
            feature_id: "admin-alert-dispatch".into(),
            remote: "origin".into(),
            provider: "gitlab".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run_id}"),
            worktree: s.roots.state.join("worktrees").join(run_id),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    event(s, run_id, "stage.completed", Some("verify"), json!({"exit_code": 0, "log": "gate.log", "commands": [["cargo", "nextest", "run"]]}));
    event(s, run_id, "review.created", Some("review_request"), json!({"id": "976", "url": "https://gitlab.example/alpha/-/merge_requests/976"}));
    event(s, run_id, "stage.completed", Some("ci"), json!({"result": "passed", "head_sha": HEAD}));
    event(s, run_id, "run.completed", None, json!({"ending": "review_ready"}));
}

fn report_log(s: &Setup, run_id: &str) -> PathBuf {
    s.roots.state.join("logs").join(run_id).join("review-report.log")
}

fn note(s: &Setup, run_id: &str, line: &str) {
    let log = report_log(s, run_id);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let mut text = std::fs::read_to_string(&log).unwrap_or_default();
    text.push_str(line);
    text.push('\n');
    std::fs::write(log, text).unwrap();
}

fn record_report(roots: &StorageRoots, run_id: &str) -> PathBuf {
    let report_path = format!("runs/{run_id}/review/{HEAD}/review.json");
    Store::open(&roots.database())
        .unwrap()
        .record_review(&ReviewRecord { run_id: run_id.into(), project: "alpha-1".into(), feature: "admin-alert-dispatch".into(), head_sha: HEAD.into(), report_path: report_path.clone() })
        .unwrap();
    roots.data.join(report_path)
}

/// Serve's provider with a report builder that records the report and keeps what it was asked to build.
fn provider(s: &Setup) -> (StoreProjects, Arc<Mutex<Vec<ReportRepair>>>) {
    let asked: Arc<Mutex<Vec<ReportRepair>>> = Default::default();
    let (kept, roots) = (asked.clone(), s.roots.clone());
    let builder = Arc::new(move |repair: &ReportRepair| {
        kept.lock().unwrap().push(repair.clone());
        Ok(record_report(&roots, &repair.run_id))
    });
    (StoreProjects::new(s.roots.clone(), s.scan.clone()).with_report_builder(builder), asked)
}

fn lifecycles(s: &Setup, run_id: &str) -> usize {
    Store::open(&s.roots.database()).unwrap().lifecycles_for_run(run_id).unwrap().len()
}

#[test]
fn a_report_that_could_not_be_built_is_built_from_the_runs_gate_and_ci_events_and_its_mr_recorded() {
    let s = setup("repair-report");
    ended_run(&s, "run-1794980659-2086195-1");
    note(&s, "run-1794980659-2086195-1", &format!("no report for {HEAD}: {NO_INDEX}"));
    let (projects, asked) = provider(&s);

    projects.repair_missing_reviews();

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!((asked[0].run_id.as_str(), asked[0].head_sha.as_str()), ("run-1794980659-2086195-1", HEAD));
    assert!(asked[0].evidence.gate_passed, "{asked:?}");
    assert_eq!(asked[0].evidence.gate_steps, Some(vec![vec!["cargo".to_string(), "nextest".into(), "run".into()]]));
    assert_eq!(asked[0].evidence.ci, Some(CiOutcome::Passed));
    assert_eq!(lifecycles(&s, "run-1794980659-2086195-1"), 1, "the MR is recorded once the report is there");
    let log = std::fs::read_to_string(report_log(&s, "run-1794980659-2086195-1")).unwrap();
    assert_eq!(log.matches("repaired by orq serve").count(), 2, "{log}");
}

#[test]
fn a_created_review_with_no_lifecycle_is_recorded_when_its_report_exists() {
    let s = setup("repair-lifecycle");
    ended_run(&s, "run-mr");
    record_report(&s.roots, "run-mr");
    note(&s, "run-mr", &format!("report for {HEAD}: somewhere"));
    let (projects, asked) = provider(&s);

    projects.repair_missing_reviews();

    assert!(asked.lock().unwrap().is_empty(), "the report was there");
    assert_eq!(lifecycles(&s, "run-mr"), 1);
    let log = std::fs::read_to_string(report_log(&s, "run-mr")).unwrap();
    assert!(log.contains("merge_requests/976") && log.contains("repaired by orq serve"), "{log}");
}

#[test]
fn repairing_again_changes_nothing() {
    let s = setup("repair-again");
    ended_run(&s, "run-again");
    note(&s, "run-again", &format!("no report for {HEAD}: {NO_INDEX}"));
    let (projects, asked) = provider(&s);
    projects.repair_missing_reviews();
    let log = std::fs::read_to_string(report_log(&s, "run-again")).unwrap();

    projects.repair_missing_reviews();

    assert_eq!(asked.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(report_log(&s, "run-again")).unwrap(), log);
    assert_eq!(lifecycles(&s, "run-again"), 1);
}

#[test]
fn a_run_still_going_or_failed_is_left_to_itself() {
    let s = setup("repair-not-ended");
    ended_run(&s, "run-failed");
    event(&s, "run-failed", "run.completed", None, json!({"ending": "failed"}));
    note(&s, "run-failed", &format!("no report for {HEAD}: {NO_INDEX}"));
    let (projects, asked) = provider(&s);
    projects.repair_missing_reviews();
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn repair_rounds_repair_at_once_and_stop_when_dropped() {
    let s = setup("repair-rounds");
    ended_run(&s, "run-round");
    note(&s, "run-round", &format!("no report for {HEAD}: {NO_INDEX}"));
    let (projects, asked) = provider(&s);
    let rounds = orq_remote::projects::start_repair_rounds(Arc::new(projects), std::time::Duration::from_secs(3600));
    // Dropping waits for the round under way: the first one runs before any wait.
    drop(rounds);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

/// A project serve sees, `alpha` under the scan root, whose runs are `run_id`'s.
fn alpha_project(s: &Setup, run_id: &str) {
    let repo = s.scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&repo).output().unwrap().status.success());
    git(&["init", "-q", "-b", "main"]);
    git(&["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
    let key = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    let mut record = s.runs.load(run_id).unwrap();
    record.project_id = key;
    std::fs::write(s.roots.state.join("runs").join(run_id).join("run.json"), serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn the_run_page_says_its_report_is_missing_and_why_and_build_report_builds_it() {
    use orq_core::remote_inbox::{ProjectsProvider, RunControl, RunControlOutcome};
    let s = setup("repair-build-now");
    ended_run(&s, "run-now");
    alpha_project(&s, "run-now");
    note(&s, "run-now", &format!("no report for {HEAD}: {NO_INDEX}"));
    let (projects, asked) = provider(&s);

    let detail = projects.run_timeline("alpha", "run-now", None).unwrap().unwrap();
    assert_eq!(detail["run"]["missing_report"]["reason"], json!(NO_INDEX), "{detail}");

    let built = projects.control_run("alpha", "run-now", RunControl::BuildReport).unwrap();
    assert!(matches!(built, RunControlOutcome::Done(ref run) if run == "run-now"));
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert_eq!(asked.lock().unwrap()[0].repo.as_deref(), Some(std::fs::canonicalize(s.scan.join("alpha")).unwrap().as_path()));
    let detail = projects.run_timeline("alpha", "run-now", None).unwrap().unwrap();
    assert!(detail["run"].get("missing_report").is_none(), "{detail}");
    assert!(std::fs::read_to_string(report_log(&s, "run-now")).unwrap().contains("repaired by orq serve"));

    let again = projects.control_run("alpha", "run-now", RunControl::BuildReport).unwrap();
    assert!(matches!(again, RunControlOutcome::Refused(_)), "nothing is missing any more");
}
