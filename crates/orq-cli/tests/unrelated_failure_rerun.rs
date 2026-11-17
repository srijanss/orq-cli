//! A fix session that reports a gate or CI failure unrelated to the run is usually looking at a flaky
//! test. Before the run blocks on it, orq runs the gate (or retries the failed CI pipeline) once more,
//! with no change: a pass carries the run on, with the flake noted; a second failure blocks as before.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, FakeAgent, FakeProvider, GitFixture, LedgerEntry};

const PR_URL: &str = "https://github.example/o/r/pull/7";
const EXIT_BLOCKED: i32 = 30;
const EXIT_FAILED: i32 = 31;
const SUMMARY: &str = "billing clock test is flaky";
const UNRELATED: &str = "the gate fails for a reason unrelated to this run";

const RED: &str = "{\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"completed\",\"conclusion\":\"failure\"}]}\n";
const GREEN: &str = "{\"state\":\"success\",\"statuses\":[],\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"completed\",\"conclusion\":\"success\"}]}\n";
/// Checks restarted by a re-run: still running, so neither the check-run nor the status call fails.
const RUNNING: &str = "{\"state\":\"pending\",\"statuses\":[],\"total_count\":1,\"check_runs\":[{\"name\":\"deploy\",\"status\":\"in_progress\",\"conclusion\":null}]}\n";
const FAILED_RUNS: &str = "[{\"databaseId\":42,\"conclusion\":\"failure\"},{\"databaseId\":41,\"conclusion\":\"success\"}]\n";

struct World {
    repo: GitFixture,
    _remote: PathBuf,
    state: PathBuf,
    config: PathBuf,
    ssh: PathBuf,
    base: PathBuf,
    provider: FakeProvider,
    _agent: FakeAgent,
    path: std::ffi::OsString,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// How the world's gate and fix sessions behave.
struct Script {
    /// The gate passes from this run of it on (counting from 1).
    gate_passes_at: u32,
    /// Whether a fix session reports the failure unrelated (else it leaves without doing anything).
    reports_unrelated: bool,
}

/// A repository with a pushed `main` and a pending COM-01, whose gate counts its runs in `gate-runs` and
/// whose fix sessions count themselves in `fix-sessions`.
fn world(tag: &str, script: Script, provider: FakeProvider) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-unrelated-rerun-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let repo = GitFixture::new();
    let remote = repo.add_bare_remote();
    git(repo.root(), &["branch", "-M", "main"]);
    git(repo.root(), &["push", "--quiet", "origin", "main"]);
    git(repo.root(), &["remote", "set-url", "origin", &format!("forge.example:{}", remote.display())]);
    let ssh = base.join("ssh");
    executable(&ssh, "#!/bin/sh\nfor a; do last=$a; done\nexec sh -c \"$last\"\n");
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);

    let count = base.join("gate-runs");
    let gate = format!("n=$(cat '{0}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{0}'; [ $n -ge {1} ]", count.display(), script.gate_passes_at);
    std::fs::create_dir_all(repo.root().join(".orq")).unwrap();
    std::fs::write(repo.root().join(".orq/run.toml"), format!("gate = [\"sh\", \"-c\", {}]\nexplain_failures = false\nci_timeout_secs = 5\n", toml_string(&gate))).unwrap();

    let agent = FakeAgent::builder().write_file("feature.txt", "done\n").commit("feat: add the feature").build();
    let report = if script.reports_unrelated { format!("echo '{SUMMARY}' > \"$ORQ_UNRELATED_FAILURE_FILE\"") } else { String::new() };
    executable(
        &base.join("bin/claude"),
        &format!(
            "#!/bin/sh\nif [ -n \"$ORQ_UNRELATED_FAILURE_FILE\" ]; then echo x >> '{}'; {report}\nexit 0; fi\nexec '{}' \"$@\"\n",
            base.join("fix-sessions").display(),
            agent.path().display()
        ),
    );
    let mut dirs = vec![base.join("bin"), provider.bin_dir()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    World { repo, _remote: remote, state: base.join("state"), config: base.join("config"), ssh, base, provider, _agent: agent, path }
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn github() -> orq_test_support::FakeProviderBuilder {
    FakeProvider::builder()
        .reply(&["pr", "list"], "[]\n")
        .reply(&["pr", "create"], &format!("{PR_URL}\n"))
        .reply(&["pr", "view"], &format!("{{\"number\":7,\"url\":\"{PR_URL}\"}}\n"))
}

/// `orq run` of COM-01 with `extra` arguments.
fn run(w: &World, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["run", "--feature", "COM-01", "--remote", "origin", "--target", "main", "--repo"])
        .arg(w.repo.root())
        .args(extra)
        .env("XDG_CONFIG_HOME", &w.config)
        .env("XDG_STATE_HOME", &w.state)
        .env("XDG_DATA_HOME", w.base.join("data"))
        .env("XDG_CACHE_HOME", w.base.join("cache"))
        .env("ORQ_FORGE", "gh")
        .env("GIT_SSH_COMMAND", &w.ssh)
        .env("PATH", &w.path)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

/// How many lines the counter file `name` holds, or the number in it.
fn counted(w: &World, name: &str) -> u32 {
    let text = std::fs::read_to_string(w.base.join(name)).unwrap_or_default();
    match name {
        "gate-runs" => text.trim().parse().unwrap_or(0),
        _ => text.lines().count() as u32,
    }
}

/// Every `kind` event the run recorded.
fn events_of(w: &World, kind: &str) -> Vec<orq_orchestrator::Event> {
    let store = orq_orchestrator::RunStore::new(&w.state.join("orq"));
    let mut found = Vec::new();
    for run in store.run_ids().unwrap() {
        found.extend(store.events(&run).unwrap().into_iter().filter(|e| e.kind == kind));
    }
    found
}

#[test]
fn a_gate_that_passes_on_its_re_run_after_an_unrelated_report_carries_the_run_on_to_merge() {
    let w = world("gate-flaky", Script { gate_passes_at: 2, reports_unrelated: true }, github().build());

    let output = run(&w, &["--no-ci-check", "--delivery", "merge-local"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(stdout.contains("state: merged"), "{stdout}\n{stderr}");
    assert_eq!(counted(&w, "fix-sessions"), 1, "the re-run starts no fix session");
    let flaky = events_of(&w, "gate.flaky");
    assert_eq!(flaky.len(), 1, "{flaky:?}");
    assert_eq!(flaky[0].stage.as_deref(), Some("verify"));
    assert_eq!(flaky[0].payload["summary"], SUMMARY);
    assert!(flaky[0].payload["step"].is_u64(), "the failing step is recorded: {:?}", flaky[0].payload);
    assert!(stderr.contains(&format!("Flaky: {SUMMARY}")), "the ending names the flake: {stderr}");
    assert!(!stderr.contains(UNRELATED), "{stderr}");
}

/// The run's page as the web Timeline fetches it.
fn run_page(w: &World) -> serde_json::Value {
    use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
    let roots = orq_core::storage::StorageRoots { data: w.base.join("data"), config: w.config.join("orq"), state: w.state.join("orq"), cache: w.base.join("cache") };
    let scan = w.repo.root().parent().unwrap().to_path_buf();
    let provider = orq_remote::projects::StoreProjects::new(roots.clone(), scan);
    let config = RemoteConfig { token: "tok".into() };
    let ask = |path: &str| {
        let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let response = handle_with(&roots, &config, "server-project", "server", &request, &provider);
        assert_eq!(response.status, 200, "{path}: {}", response.body);
        response.body
    };
    let name = w.repo.root().file_name().unwrap().to_str().unwrap().to_string();
    let projects = ask("/api/projects");
    let id = projects["projects"].as_array().unwrap().iter().find(|p| p["name"] == name.as_str()).unwrap_or_else(|| panic!("{name} not listed: {projects}"))["id"].as_str().unwrap().to_string();
    let run = orq_orchestrator::RunStore::new(&w.state.join("orq")).run_ids().unwrap().pop().unwrap();
    ask(&format!("/api/projects/{id}/runs/{run}"))
}

#[test]
fn the_web_timeline_of_a_run_whose_gate_passed_on_its_re_run_carries_the_flaky_note() {
    let w = world("gate-flaky-web", Script { gate_passes_at: 2, reports_unrelated: true }, github().build());

    let output = run(&w, &["--no-ci-check", "--delivery", "merge-local"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let page = run_page(&w);
    let flaky: Vec<&serde_json::Value> = page["events"].as_array().unwrap().iter().filter(|e| e["kind"] == "gate.flaky").collect();
    assert_eq!(flaky.len(), 1, "{page}");
    assert_eq!(flaky[0]["stage"], "verify");
    assert_eq!(flaky[0]["flaky"], serde_json::json!({"summary": SUMMARY, "passed": true}), "{page}");
}

#[test]
fn a_gate_that_fails_its_re_run_too_blocks_the_run_after_one_re_run() {
    let w = world("gate-red", Script { gate_passes_at: 1000, reports_unrelated: true }, github().build());

    let output = run(&w, &["--no-ci-check"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{stdout}\n{stderr}");
    assert!(stdout.contains("state: blocked"), "{stdout}");
    assert!(stderr.contains(&format!("{UNRELATED}: {SUMMARY}")), "today's message: {stderr}");
    assert!(stderr.contains("the re-run failed too"), "{stderr}");
    assert_eq!(counted(&w, "gate-runs"), 2, "the first run and one re-run");
    assert_eq!(counted(&w, "fix-sessions"), 1, "the re-run starts no fix session");
    assert_eq!(events_of(&w, "gate.rerun_failed").len(), 1);
    assert!(events_of(&w, "gate.flaky").is_empty());
    assert!(!stderr.contains("Flaky:"), "{stderr}");
}

#[test]
fn a_gate_that_fails_without_an_unrelated_report_is_never_re_run_blindly() {
    let w = world("gate-plain", Script { gate_passes_at: 1000, reports_unrelated: false }, github().build());

    let output = run(&w, &["--no-ci-check"]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(EXIT_FAILED), "{stdout}\n{stderr}");
    let fixes = counted(&w, "fix-sessions");
    assert!(fixes >= 1, "the fix sessions run as before");
    assert_eq!(counted(&w, "gate-runs"), fixes + 1, "one gate run per fix session, and no more");
    assert!(events_of(&w, "gate.flaky").is_empty() && events_of(&w, "gate.rerun_failed").is_empty());
    assert!(!stderr.contains("re-run"), "{stderr}");
}

/// The `gh run rerun` calls the run made.
fn reruns(w: &World) -> Vec<Vec<String>> {
    w.provider.calls().into_iter().filter(|call| call.args.starts_with(&["run".to_string(), "rerun".to_string()])).map(|call| call.args).collect()
}

#[test]
fn a_ci_pipeline_reported_unrelated_is_retried_once_and_a_pass_carries_the_run_on() {
    let provider = github().reply_once(&["api"], RED).reply(&["api"], GREEN).reply(&["run", "list"], FAILED_RUNS).reply(&["run", "rerun"], "").build();
    let w = world("ci-flaky", Script { gate_passes_at: 1, reports_unrelated: true }, provider);

    let output = run(&w, &[]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(stdout.contains("state: review_ready"), "{stdout}\n{stderr}");
    let reruns = reruns(&w);
    assert_eq!(reruns.len(), 1, "only the failed workflow run is retried, once: {reruns:?}");
    assert!(reruns[0].contains(&"42".to_string()) && reruns[0].contains(&"--failed".to_string()), "{reruns:?}");
    assert_eq!(counted(&w, "fix-sessions"), 1);
    let flaky = events_of(&w, "ci.flaky");
    assert_eq!(flaky.len(), 1, "{flaky:?}");
    assert_eq!(flaky[0].stage.as_deref(), Some("ci"));
    assert_eq!(flaky[0].payload["summary"], SUMMARY);
    assert!(stderr.contains(&format!("Flaky: {SUMMARY}")), "{stderr}");
}

#[test]
fn a_ci_pipeline_that_fails_its_retry_too_blocks_the_run() {
    // CI fails; the re-run is seen running (one check-run and one status call), then fails too.
    let provider = github().reply_once(&["api"], RED).reply_once(&["api"], RUNNING).reply_once(&["api"], RUNNING).reply(&["api"], RED).reply(&["run", "list"], FAILED_RUNS).reply(&["run", "rerun"], "").build();
    let w = world("ci-red", Script { gate_passes_at: 1, reports_unrelated: true }, provider);

    let output = run(&w, &[]);

    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(EXIT_BLOCKED), "{stdout}\n{stderr}");
    assert!(stderr.contains(&format!("CI fails for a reason unrelated to this run: {SUMMARY}")), "{stderr}");
    assert!(stderr.contains("the re-run failed too"), "{stderr}");
    assert_eq!(reruns(&w).len(), 1);
    assert_eq!(counted(&w, "fix-sessions"), 1, "the retry starts no fix session");
    assert_eq!(events_of(&w, "ci.rerun_failed").len(), 1);
}
