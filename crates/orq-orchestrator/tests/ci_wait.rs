use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use orq_orchestrator::{wait_for_ci, CiOutcome, CiWaitConfig, RunRecord, RunStore};
use orq_test_support::{FakeProvider, FakeProviderBuilder};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct World {
    store: RunStore,
}

fn world(tag: &str, provider: &str, remote_url: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-ci-wait-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    for args in [vec!["init", "-q", "-b", "main"], vec!["remote", "add", "origin", remote_url]] {
        let out = std::process::Command::new("git").arg("-C").arg(&worktree).args(&args).output().unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: provider.to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store }
}

fn config() -> CiWaitConfig {
    CiWaitConfig {
        enabled: true,
        poll_interval: Duration::from_millis(20),
        timeout: Duration::from_secs(2),
        command_timeout: Duration::from_secs(10),
    }
}

fn kinds(w: &World) -> Vec<String> {
    w.store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect()
}

#[test]
fn all_check_runs_green_for_the_pushed_sha_passes_the_stage() {
    let w = world("green", "github", "https://github.example/o/r.git");
    let provider = github_provider()
        .reply(
            &["api"],
            "{\"total_count\":2,\"check_runs\":[\
             {\"name\":\"build\",\"status\":\"completed\",\"conclusion\":\"success\"},\
             {\"name\":\"lint\",\"status\":\"completed\",\"conclusion\":\"neutral\"}]}\n",
        )
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed);
    let calls = provider.calls();
    assert_eq!(calls.len(), 2, "the check runs and the commit statuses are both asked about");
    let args = calls[0].args.join("\u{1}");
    assert!(args.contains(&format!("repos/o/r/commits/{SHA}/check-runs")), "the pushed sha is asked about: {args}");
    assert!(args.contains("--hostname\u{1}github.example"), "{args}");
    assert_eq!(kinds(&w), ["stage.started", "stage.completed"]);
    let done = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(done.stage.as_deref(), Some("ci"));
    assert_eq!(done.payload["result"], "passed");
    assert_eq!(done.payload["head_sha"], SHA);
}

const NO_STATUSES: &str = "{\"state\":\"pending\",\"total_count\":0,\"statuses\":[]}\n";

/// The args of the GitHub commit-status call for `SHA` on github.example/o/r.
fn status_call() -> Vec<String> {
    ["api", "--hostname", "github.example", &format!("repos/o/r/commits/{SHA}/status")].map(String::from).to_vec()
}

/// A GitHub fake whose commit-status call reports no statuses at all.
fn github_provider() -> FakeProviderBuilder {
    let call = status_call();
    let prefix: Vec<&str> = call.iter().map(String::as_str).collect();
    FakeProvider::builder().reply(&prefix, NO_STATUSES)
}

fn gh_status(state: &str, rows: &str, total: usize) -> String {
    format!("{{\"state\":\"{state}\",\"total_count\":{total},\"statuses\":[{rows}]}}\n")
}

fn gh_checks(rows: &str, total: usize) -> String {
    format!("{{\"total_count\":{total},\"check_runs\":[{rows}]}}\n")
}

const GREEN: &str = "{\"name\":\"build\",\"status\":\"completed\",\"conclusion\":\"success\"}";
const RED: &str = "{\"name\":\"te\\u0007st\",\"status\":\"completed\",\"conclusion\":\"failure\"}";
const RUNNING: &str = "{\"name\":\"slow\",\"status\":\"in_progress\",\"conclusion\":null}";

#[test]
fn a_disabled_stage_asks_the_provider_nothing_and_is_recorded_as_skipped() {
    let w = world("off", "github", "https://github.example/o/r.git");
    let provider = FakeProvider::builder().build();
    let off = CiWaitConfig { enabled: false, ..config() };

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &off).unwrap();

    assert_eq!(outcome, CiOutcome::Skipped);
    assert!(provider.calls().is_empty());
    assert_eq!(kinds(&w), ["stage.skipped"]);
}

#[test]
fn a_red_check_fails_the_stage_naming_the_check_without_control_characters() {
    let w = world("red", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks(&[GREEN, RED].join(","), 2)).build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Failed { failing: vec!["test".to_string()] });
    assert_eq!(kinds(&w), ["stage.started", "stage.failed"]);
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(failed.payload["result"], "failed");
}

#[test]
fn checks_that_never_finish_time_out_after_being_polled_repeatedly() {
    let w = world("slow", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks(RUNNING, 1)).build();
    let short = CiWaitConfig { timeout: Duration::from_millis(300), ..config() };

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &short).unwrap();

    assert_eq!(outcome, CiOutcome::TimedOut);
    assert!(provider.calls().len() > 1, "polled more than once");
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(failed.payload["result"], "timed_out");
}

#[test]
fn a_commit_without_any_check_is_waited_for_not_called_green() {
    let w = world("none", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks("", 0)).build();
    let short = CiWaitConfig { timeout: Duration::from_millis(200), ..config() };

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &short).unwrap();

    assert_eq!(outcome, CiOutcome::TimedOut);
}

#[test]
fn a_provider_that_keeps_erroring_is_recorded_as_an_error_not_as_green() {
    let w = world("err", "github", "https://github.example/o/r.git");
    let provider = github_provider().fail(&["api"], 1, "HTTP 502\n").build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert!(matches!(outcome, CiOutcome::Errored { .. }), "{outcome:?}");
    assert_eq!(provider.calls().len(), 3, "a few polls are tolerated before giving up");
    let failed = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(failed.payload["result"], "errored");
}

#[test]
fn a_gitlab_pipeline_for_the_pushed_sha_is_waited_on_through_the_api() {
    let w = world("glab", "gitlab", "https://gitlab.example/group/sub/r.git");
    let body = format!("[{{\"id\":4,\"sha\":\"{SHA}\",\"status\":\"success\"}}]\n");
    let provider = FakeProvider::builder().reply(&["api"], &body).build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.glab(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed);
    let args = provider.calls()[0].args.join("\u{1}");
    assert!(args.contains(&format!("projects/group%2Fsub%2Fr/pipelines?sha={SHA}")), "{args}");
    assert!(args.contains("--hostname\u{1}gitlab.example"), "{args}");
}

#[test]
fn a_commit_id_that_is_not_hex_is_refused_before_any_request_is_made() {
    let w = world("badsha", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks(GREEN, 1)).build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), "../../etc?x=1", &config()).unwrap();

    assert!(matches!(outcome, CiOutcome::Errored { .. }), "{outcome:?}");
    assert!(provider.calls().is_empty());
}

#[test]
fn an_unknown_run_is_an_error() {
    let w = world("ghost", "github", "https://github.example/o/r.git");
    let provider = FakeProvider::builder().build();

    assert!(wait_for_ci(&w.store, "ghost", &provider.gh(), SHA, &config()).is_err());
}

#[test]
fn a_newer_gitlab_pipeline_without_a_status_is_not_skipped_in_favour_of_an_older_green_one() {
    let w = world("glab-malformed", "gitlab", "https://gitlab.example/group/sub/r.git");
    let body = format!("[{{\"id\":4,\"sha\":\"{SHA}\",\"status\":\"success\"}},{{\"id\":9,\"sha\":\"{SHA}\"}}]\n");
    let provider = FakeProvider::builder().reply(&["api"], &body).build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.glab(), SHA, &config()).unwrap();

    assert!(matches!(outcome, CiOutcome::Errored { .. }), "{outcome:?}");
}

#[test]
fn a_provider_call_that_outlasts_the_stage_timeout_is_cut_short_and_never_passes() {
    let w = world("slow-call", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply_slowly(&["api"], &gh_checks(GREEN, 1), 4000).build();
    let short = CiWaitConfig { timeout: Duration::from_millis(500), command_timeout: Duration::from_secs(30), ..config() };
    let started = std::time::Instant::now();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &short).unwrap();

    assert_eq!(outcome, CiOutcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3), "the stage timeout bounds the call: {:?}", started.elapsed());
}

#[test]
fn the_stage_waits_out_its_whole_timeout_even_when_the_poll_interval_is_longer() {
    let w = world("long-interval", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks(RUNNING, 1)).build();
    let cfg = CiWaitConfig { poll_interval: Duration::from_millis(900), timeout: Duration::from_millis(400), ..config() };
    let started = std::time::Instant::now();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &cfg).unwrap();

    assert_eq!(outcome, CiOutcome::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(390), "{:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_millis(850), "the sleep is capped to the time left: {:?}", started.elapsed());
}

#[test]
fn transient_provider_errors_are_forgiven_once_a_poll_succeeds() {
    let w = world("recover", "github", "https://github.example/o/r.git");
    let provider = github_provider()
        .fail_once(&["api"], 1, "HTTP 502\n")
        .fail_once(&["api"], 1, "HTTP 502\n")
        .reply(&["api"], &gh_checks(GREEN, 1))
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed);
    assert_eq!(provider.calls().len(), 4);
    assert_eq!(kinds(&w), ["stage.started", "stage.completed"]);
}

#[test]
fn checks_that_finish_while_being_polled_pass_the_stage() {
    let w = world("pending-then-green", "github", "https://github.example/o/r.git");
    let provider = github_provider()
        .reply_once(&["api"], &gh_checks(RUNNING, 1))
        .reply_once(&["api"], &gh_checks("", 0))
        .reply(&["api"], &gh_checks(GREEN, 1))
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed);
    assert_eq!(provider.calls().len(), 6, "three polls, each asking for check runs and statuses");
}

#[test]
fn a_usable_pending_answer_resets_the_count_of_consecutive_provider_errors() {
    let w = world("reset", "github", "https://github.example/o/r.git");
    let provider = github_provider()
        .fail_once(&["api"], 1, "HTTP 502\n")
        .fail_once(&["api"], 1, "HTTP 502\n")
        .reply_once(&["api"], &gh_checks(RUNNING, 1))
        .fail_once(&["api"], 1, "HTTP 502\n")
        .fail_once(&["api"], 1, "HTTP 502\n")
        .reply(&["api"], &gh_checks(GREEN, 1))
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed, "four errors in all, but never three in a row");
    assert_eq!(provider.calls().len(), 8);
}

#[test]
fn a_zero_poll_interval_does_not_hammer_the_provider() {
    let w = world("zero-interval", "github", "https://github.example/o/r.git");
    let provider = github_provider().reply(&["api"], &gh_checks(RUNNING, 1)).build();
    let cfg = CiWaitConfig { poll_interval: Duration::ZERO, timeout: Duration::from_millis(500), ..config() };

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &cfg).unwrap();

    assert_eq!(outcome, CiOutcome::TimedOut);
    assert!(provider.calls().len() <= 24, "polled {} times in half a second", provider.calls().len());
}

#[test]
fn a_failing_commit_status_fails_the_stage_even_when_every_check_run_is_green() {
    let w = world("status-red", "github", "https://github.example/o/r.git");
    let call = status_call();
    let prefix: Vec<&str> = call.iter().map(String::as_str).collect();
    let provider = FakeProvider::builder()
        .reply(&["api"], &gh_checks(GREEN, 1))
        .reply(&prefix, &gh_status("failure", "{\"context\":\"ci/legacy\",\"state\":\"failure\"}", 1))
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Failed { failing: vec!["ci/legacy".to_string()] });
}

#[test]
fn a_pending_commit_status_keeps_the_stage_waiting_and_a_green_one_alone_passes_it() {
    let w = world("status-pending", "github", "https://github.example/o/r.git");
    let call = status_call();
    let prefix: Vec<&str> = call.iter().map(String::as_str).collect();
    let pending = gh_status("pending", "{\"context\":\"ci/legacy\",\"state\":\"pending\"}", 1);
    let green = gh_status("success", "{\"context\":\"ci/legacy\",\"state\":\"success\"}", 1);
    let provider = FakeProvider::builder()
        .reply(&["api"], &gh_checks("", 0))
        .reply_once(&prefix, &pending)
        .reply(&prefix, &green)
        .build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert_eq!(outcome, CiOutcome::Passed, "statuses alone are enough once they are green");
    assert!(provider.calls().len() >= 4, "the pending status was waited out");
}

#[test]
fn an_unreadable_commit_status_is_never_green() {
    let w = world("status-bad", "github", "https://github.example/o/r.git");
    let call = status_call();
    let prefix: Vec<&str> = call.iter().map(String::as_str).collect();
    let provider = FakeProvider::builder().reply(&["api"], &gh_checks(GREEN, 1)).reply(&prefix, "not json\n").build();

    let outcome = wait_for_ci(&w.store, "run-1", &provider.gh(), SHA, &config()).unwrap();

    assert!(matches!(outcome, CiOutcome::Errored { .. }), "{outcome:?}");
}
