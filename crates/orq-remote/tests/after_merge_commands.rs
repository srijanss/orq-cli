//! A project whose `.orq/run.toml` has `[after_merge] commands = [[argv], ...]` has them run in its main
//! checkout once the queue settles a merge-local run's merge: in order, stopping at the first failure,
//! at most once per merge, and recorded on the run as `after_merge.commands`. Each round is driven here
//! with a fake launcher against a temporary project; the commands are fakes that write marker files.

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
use orq_remote::projects::{LaunchError, QueueRound, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Starts a merge-local run the way `orq run` records one.
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
    /// The project's main checkout, on `main`.
    repo: PathBuf,
    /// Where the fake commands leave their marks.
    marks: PathBuf,
}

/// A project `alpha` on `main` with features COM-01 and COM-02 queued, delivered by merge-local, whose
/// `.orq/run.toml` is `run_toml` when it is given.
fn setup(label: &str, run_toml: Option<&str>) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher {
        state: roots.state.clone(),
        started: Mutex::default(),
    });
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "start\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".tdd-*.json\n.claude/\n.orq/\n").unwrap();
    git(&repo, &["add", "README", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    let ledger = json!([{"featureName": "COM-01", "dependsOn": [], "status": "pending"}, {"featureName": "COM-02", "dependsOn": [], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    if let Some(text) = run_toml {
        std::fs::create_dir_all(repo.join(".orq")).unwrap();
        std::fs::write(repo.join(".orq/run.toml"), text).unwrap();
    }
    let marks = base.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let settings = ProjectSettings {
        delivery: Delivery::MergeLocal,
        max_active_runs: Some(1),
        ..ProjectSettings::default()
    };
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup {
        scan,
        roots,
        launcher,
        repo,
        marks,
    };
    let (status, body) = send(
        &s,
        "POST",
        "/api/projects/alpha/queue",
        json!({"features": ["COM-01", "COM-02"]})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    s
}

/// A `run.toml` whose `[after_merge]` commands each append their name to `marks/order`, or fail when the
/// name starts with `fail-`.
fn run_toml(s_marks: &Path, names: &[&str]) -> String {
    let commands: Vec<String> = names
        .iter()
        .map(|name| {
            let script = if name.starts_with("fail-") {
                format!("echo {name} >> {0}/order; exit 3", s_marks.display())
            } else {
                format!("echo {name} >> {0}/order", s_marks.display())
            };
            format!("[\"sh\", \"-c\", \"{script}\"]")
        })
        .collect();
    format!("[after_merge]\ncommands = [{}]\n", commands.join(", "))
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone())
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let r = handle_with_body(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        body,
        &provider(s),
    );
    (r.status, r.body)
}

/// One round of the queue at noon, with no restart pending.
fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound {
        minute_of_day: 12 * 60,
        restart_pending: false,
    });
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

/// A merge-local run of `feature` in the project at `repo`, still running.
fn seed_run(state: &Path, repo: &Path, run: &str, feature: &str) {
    let store = RunStore::new(state);
    let record = RunRecord {
        delivery: Delivery::MergeLocal,
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

/// The run's merge lands: `merge.completed` at `head`, then the run ends merged.
fn merge(s: &Setup, run: &str, head: &str) {
    let store = RunStore::new(&s.roots.state);
    event(
        &store,
        run,
        "merge.completed",
        json!({"head_sha": head, "target_branch": "main"}),
    );
    event(&store, run, "run.completed", json!({"ending": "merged"}));
}

/// Runs git in `dir` and gives back what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The newest event of `kind` on `run`, waiting up to ten seconds for it.
fn wait_for(s: &Setup, run: &str, kind: &str) -> Option<Event> {
    let store = RunStore::new(&s.roots.state);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = store
            .events(run)
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.kind == kind)
        {
            return Some(found);
        }
        if Instant::now() > until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn order(s: &Setup) -> Vec<String> {
    std::fs::read_to_string(s.marks.join("order"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Starts COM-01, lets a round see it going, and lands its merge at the checkout's HEAD; the run's id
/// and the merged sha. The round that then settles it is left to the caller.
fn merged_run(s: &Setup) -> (String, String) {
    round(s);
    round(s);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    let run = "run-1-COM-01".to_string();
    merge(s, &run, &head);
    (run, head)
}

#[test]
fn the_commands_run_in_order_in_the_main_checkout_and_the_run_records_them() {
    let s = setup("in-order", None);
    std::fs::create_dir_all(s.repo.join(".orq")).unwrap();
    std::fs::write(
        s.repo.join(".orq/run.toml"),
        run_toml(&s.marks, &["one", "two"]),
    )
    .unwrap();
    let (run, head) = merged_run(&s);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("the commands were recorded");
    assert_eq!(order(&s), ["one", "two"]);
    assert_eq!(done.payload["ran"], true);
    assert_eq!(done.payload["merged"], head.as_str());
    let commands = done.payload["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["exit_code"], 0);
    assert_eq!(commands[1]["exit_code"], 0);
    assert_eq!(commands[0]["argv"][0], "sh");
}

/// Writes `text` as the project's `.orq/run.toml`.
fn write_run_toml(s: &Setup, text: &str) {
    std::fs::create_dir_all(s.repo.join(".orq")).unwrap();
    std::fs::write(s.repo.join(".orq/run.toml"), text).unwrap();
}

/// Waits long enough for a go that would have started to have done so.
fn settle_time() {
    std::thread::sleep(Duration::from_millis(1500));
}

fn events_of(s: &Setup, run: &str, kind: &str) -> Vec<Event> {
    RunStore::new(&s.roots.state)
        .events(run)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[test]
fn a_failing_command_skips_the_ones_after_it() {
    let s = setup("fail-first", None);
    write_run_toml(&s, &run_toml(&s.marks, &["fail-one", "two"]));
    let (run, _) = merged_run(&s);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert_eq!(order(&s), ["fail-one"], "the second command never ran");
    assert_eq!(done.payload["ok"], false);
    let commands = done.payload["commands"].as_array().unwrap();
    assert_eq!(commands[0]["exit_code"], 3);
    assert_eq!(commands[1]["skipped"], true);
    assert!(commands[1].get("exit_code").is_none());
}

#[test]
fn a_command_that_cannot_start_is_a_failure_and_stops_the_rest() {
    let s = setup("no-such-program", None);
    write_run_toml(&s, &format!("[after_merge]\ncommands = [[\"/no/such/program\"], [\"sh\", \"-c\", \"echo two >> {}/order\"]]\n", s.marks.display()));
    let (run, _) = merged_run(&s);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert!(order(&s).is_empty());
    let commands = done.payload["commands"].as_array().unwrap();
    assert!(
        commands[0]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{commands:?}"
    );
    assert_eq!(commands[1]["skipped"], true);
}

#[test]
fn a_merge_the_server_did_not_watch_land_gets_no_go() {
    // The run's merge is first seen by the round that settles it: the server started after the merge.
    let s = setup("unwatched", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    round(&s);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    merge(&s, "run-1-COM-01", &head);
    round(&s);
    settle_time();
    assert!(order(&s).is_empty());
    assert!(events_of(&s, "run-1-COM-01", "after_merge.started").is_empty());
    assert!(events_of(&s, "run-1-COM-01", "after_merge.commands").is_empty());
}

#[test]
fn a_project_that_gained_after_merge_after_the_run_began_gets_no_go() {
    let s = setup("added-later", None);
    round(&s);
    round(&s);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    merge(&s, "run-1-COM-01", &head);
    round(&s);
    settle_time();
    assert!(order(&s).is_empty());
    assert!(events_of(&s, "run-1-COM-01", "after_merge.commands").is_empty());
}

#[test]
fn a_run_that_already_recorded_its_go_gets_none() {
    let s = setup("already-recorded", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    let (run, _) = merged_run(&s);
    event(
        &RunStore::new(&s.roots.state),
        &run,
        "after_merge.commands",
        json!({"ran": true, "ok": true, "commands": []}),
    );
    round(&s);
    settle_time();
    assert!(order(&s).is_empty());
    assert_eq!(events_of(&s, &run, "after_merge.commands").len(), 1);
}

#[test]
fn restarting_serve_or_editing_the_settings_replays_nothing() {
    let s = setup("no-replay", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    let (run, _) = merged_run(&s);
    round(&s);
    wait_for(&s, &run, "after_merge.commands").expect("recorded");
    write_run_toml(&s, &run_toml(&s.marks, &["one", "edited"]));
    round(&s);
    round(&s);
    settle_time();
    assert_eq!(order(&s), ["one"]);
    assert_eq!(events_of(&s, &run, "after_merge.commands").len(), 1);
}

#[test]
fn a_checkout_on_another_branch_is_skipped_with_the_reason() {
    let s = setup("other-branch", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    let (run, _) = merged_run(&s);
    git(&s.repo, &["checkout", "-q", "-b", "elsewhere"]);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert!(order(&s).is_empty());
    assert_eq!(done.payload["ran"], false);
    assert_eq!(done.payload["skipped_reason"], "checkout on elsewhere");
    assert_eq!(done.payload["commands"][0]["skipped"], true);
}

#[test]
fn a_checkout_not_at_the_merged_commit_is_skipped_with_both_shas() {
    let s = setup("not-merged", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    round(&s);
    round(&s);
    let before = git(&s.repo, &["rev-parse", "HEAD"]);
    std::fs::write(s.repo.join("later.txt"), "later\n").unwrap();
    git(&s.repo, &["add", "later.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "the merge"]);
    let merged = git(&s.repo, &["rev-parse", "HEAD"]);
    git(&s.repo, &["reset", "-q", "--hard", &before]);
    merge(&s, "run-1-COM-01", &merged);
    round(&s);
    let done = wait_for(&s, "run-1-COM-01", "after_merge.commands").expect("recorded");
    assert!(order(&s).is_empty());
    assert_eq!(
        done.payload["skipped_reason"],
        format!("HEAD is {before}, not {merged}")
    );
}

#[test]
fn a_checkout_on_a_commit_after_the_merged_one_runs_the_commands() {
    let s = setup("release-commit", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    let (run, _) = merged_run(&s);
    std::fs::write(s.repo.join("release.txt"), "release\n").unwrap();
    git(&s.repo, &["add", "release.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "release"]);
    round(&s);
    wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert_eq!(order(&s), ["one"]);
}

#[test]
fn started_is_written_before_the_first_command_and_a_slow_command_does_not_hold_the_round() {
    let s = setup("slow", None);
    write_run_toml(
        &s,
        &format!(
            "[after_merge]\ncommands = [[\"sh\", \"-c\", \"sleep 2; echo slow >> {}/order\"]]\n",
            s.marks.display()
        ),
    );
    let (run, merged) = merged_run(&s);
    let began = Instant::now();
    round(&s);
    assert!(
        began.elapsed() < Duration::from_millis(1800),
        "the round waited for the command: {:?}",
        began.elapsed()
    );
    let started = events_of(&s, &run, "after_merge.started");
    assert_eq!(
        started.len(),
        1,
        "started is on the run before the command ends"
    );
    assert_eq!(started[0].payload["merged"], merged.as_str());
    assert!(
        events_of(&s, &run, "after_merge.commands").is_empty(),
        "still running"
    );
    wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert_eq!(order(&s), ["slow"]);
}

#[test]
fn two_merges_in_a_project_never_run_their_commands_at_once() {
    let s = setup("serial", None);
    let marks = s.marks.display();
    write_run_toml(&s, &format!("[after_merge]\ncommands = [[\"sh\", \"-c\", \"echo start >> {marks}/order; sleep 2; echo end >> {marks}/order\"]]\n"));
    let (first, head) = merged_run(&s);
    round(&s);
    // COM-02's run began on that round; it lands its merge while the first go still runs.
    round(&s);
    merge(&s, "run-2-COM-02", &head);
    round(&s);
    wait_for(&s, &first, "after_merge.commands").expect("first recorded");
    wait_for(&s, "run-2-COM-02", "after_merge.commands").expect("second recorded");
    assert_eq!(order(&s), ["start", "end", "start", "end"]);
}

#[test]
fn a_go_the_server_stopped_in_the_middle_of_is_closed_as_interrupted_and_not_run_again() {
    let s = setup("interrupted", None);
    write_run_toml(&s, &run_toml(&s.marks, &["one"]));
    seed_run(&s.roots.state, &s.repo, "run-old-COM-01", "COM-01");
    event(
        &RunStore::new(&s.roots.state),
        "run-old-COM-01",
        "after_merge.started",
        json!({"merged": "abc", "commands": 1}),
    );
    round(&s);
    let done = wait_for(&s, "run-old-COM-01", "after_merge.commands").expect("closed");
    assert_eq!(done.payload["interrupted"], true);
    assert_eq!(done.payload["ran"], false);
    round(&s);
    settle_time();
    assert!(order(&s).is_empty(), "nothing was run again");
    assert_eq!(
        events_of(&s, "run-old-COM-01", "after_merge.commands").len(),
        1
    );
}

#[test]
fn a_bad_shape_is_reported_on_the_run_and_runs_nothing() {
    let s = setup("bad-shape", None);
    write_run_toml(&s, "[after_merge]\ncommands = \"make install\"\n");
    let (run, _) = merged_run(&s);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert_eq!(done.payload["ran"], false);
    assert!(
        done.payload["error"]
            .as_str()
            .unwrap()
            .contains("after_merge.commands"),
        "{}",
        done.payload
    );
}

#[test]
fn the_argv_in_the_event_is_redacted() {
    let s = setup("redacted", None);
    write_run_toml(&s, "[after_merge]\ncommands = [[\"echo\", \"token=ghp_abcdefghijklmnopqrstuvwxyz0123456789\"]]\n");
    let (run, _) = merged_run(&s);
    round(&s);
    let done = wait_for(&s, &run, "after_merge.commands").expect("recorded");
    assert!(
        !done
            .payload
            .to_string()
            .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{}",
        done.payload
    );
}

#[test]
fn after_merge_commands_are_argv_lists_and_a_bad_shape_names_its_key() {
    use orq_core::project_settings::AfterMergeCommands;
    assert_eq!(
        AfterMergeCommands::parse("").unwrap(),
        None,
        "no table means no commands"
    );
    assert_eq!(AfterMergeCommands::parse("[other]\nx = 1\n").unwrap(), None);
    let parsed = AfterMergeCommands::parse(
        "[after_merge]\ncommands = [[\"cargo\", \"build\"], [\"true\"]]\n",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        parsed.commands,
        vec![
            vec!["cargo".to_string(), "build".to_string()],
            vec!["true".to_string()]
        ]
    );
    assert_eq!(
        AfterMergeCommands::parse("[after_merge]\n")
            .unwrap()
            .unwrap()
            .commands,
        Vec::<Vec<String>>::new()
    );
    for (text, key) in [
        (
            "[after_merge]\ncommands = \"cargo build\"\n",
            "after_merge.commands",
        ),
        (
            "[after_merge]\ncommands = [\"cargo build\"]\n",
            "after_merge.commands[0]",
        ),
        (
            "[after_merge]\ncommands = [[\"ok\"], []]\n",
            "after_merge.commands[1]",
        ),
        (
            "[after_merge]\ncommands = [[\"ok\", 3]]\n",
            "after_merge.commands[0][1]",
        ),
        (
            "[after_merge]\ncommands = [[\"\"]]\n",
            "after_merge.commands[0]",
        ),
        ("[after_merge\n", "run.toml"),
    ] {
        let error = AfterMergeCommands::parse(text).unwrap_err();
        assert!(error.contains(key), "{text:?}: {error}");
    }
}
