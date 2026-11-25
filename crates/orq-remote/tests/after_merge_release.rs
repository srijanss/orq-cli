//! A merge-local merge that no round of the queue settled (the server restarted the moment the merging
//! run ended) is found by the next server's rounds: released and given its `[after_merge]` go once, or,
//! when a release commit already follows it on the target, recorded as not ran with that release. Driven
//! here with a fake agent and fake commands against a temporary project; nothing is queued.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{QueueRound, ReleaseAgent, StoreProjects};
use serde_json::{json, Value};

/// An agent that always answers patch, counting its calls.
#[derive(Default)]
struct FakeAgent {
    asked: Mutex<usize>,
}

impl ReleaseAgent for FakeAgent {
    fn ask(&self, _harness: &str, _argv: &[String]) -> Result<String, String> {
        *self.asked.lock().unwrap() += 1;
        Ok(json!({"level": "patch", "reason": "because patch"}).to_string())
    }
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    agent: Arc<FakeAgent>,
    repo: PathBuf,
    marks: PathBuf,
}

/// A project `alpha` on `main` at version 0.1.2, merge-local, whose `.orq/run.toml` is `run_toml` (with
/// `{marks}` standing for where the fake commands write).
fn setup(label: &str, run_toml: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    let marks = base.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.1.2\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        ".tdd-*.json\n.claude/\n.orq/\nCargo.lock\n",
    )
    .unwrap();
    git(&repo, &["add", "Cargo.toml", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    std::fs::write(
        repo.join(".tdd-features.json"),
        json!([{"featureName": "COM-01", "dependsOn": [], "status": "pending"}]).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(repo.join(".orq")).unwrap();
    std::fs::write(
        repo.join(".orq/run.toml"),
        run_toml.replace("{marks}", &marks.display().to_string()),
    )
    .unwrap();
    let settings = ProjectSettings {
        delivery: Delivery::MergeLocal,
        ..ProjectSettings::default()
    };
    Store::open(&roots.database())
        .unwrap()
        .save_project_settings(&repo, 0, &settings)
        .unwrap();
    Setup {
        scan,
        roots,
        agent: Arc::default(),
        repo,
        marks,
    }
}

/// Releases on merge and has one `[after_merge]` command that appends `after` to `marks/order`.
const RELEASING: &str = "[release]\nrelease_on_merge = \"auto\"\ncommands = [[\"sh\", \"-c\", \"true\"]]\n\n[after_merge]\ncommands = [[\"sh\", \"-c\", \"echo after >> {marks}/order\"]]\n";

/// A new server: a fresh provider over the same state, as `orq serve` makes after a restart.
fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_release_agent(s.agent.clone())
}

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
            occurred_at: "2026-11-25T15:42:00Z".into(),
            payload,
        })
        .unwrap();
}

/// A merge-local run of COM-01 whose merge landed as a commit of `subject` on main and which ended merged,
/// without any round seeing it; its id and the merged sha.
fn merged_unseen(s: &Setup, run: &str, subject: &str) -> String {
    let store = RunStore::new(&s.roots.state);
    let record = RunRecord {
        delivery: Delivery::MergeLocal,
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(&s.repo).unwrap()),
        feature_id: "COM-01".into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: git(&s.repo, &["rev-parse", "refs/heads/main"]),
        branch: format!("orq/{run}"),
        worktree: s.repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
    std::fs::write(s.repo.join(format!("{run}.txt")), subject).unwrap();
    git(&s.repo, &["add", "."]);
    git(&s.repo, &["commit", "-q", "-m", subject]);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    event(
        &store,
        run,
        "merge.completed",
        json!({"head_sha": head, "target_branch": "main"}),
    );
    event(&store, run, "run.completed", json!({"ending": "merged"}));
    head
}

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

fn wait_for(s: &Setup, run: &str, kind: &str) -> Option<Event> {
    let store = RunStore::new(&s.roots.state);
    let until = Instant::now() + Duration::from_secs(15);
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

fn events_of(s: &Setup, run: &str, kind: &str) -> Vec<Event> {
    RunStore::new(&s.roots.state)
        .events(run)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn order(s: &Setup) -> Vec<String> {
    std::fs::read_to_string(s.marks.join("order"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Waits long enough for a go that would have started to have done so.
fn settle_time() {
    std::thread::sleep(Duration::from_millis(1500));
}

#[test]
fn a_merge_no_round_settled_is_released_and_its_commands_run_once_by_the_next_server() {
    let s = setup("unsettled-released", RELEASING);
    let merged = merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    round(&s);
    let created = wait_for(&s, "run-1-COM-01", "release.created").expect("the release was made");
    assert_eq!(created.payload["version"], "0.1.3");
    assert_eq!(created.payload["merged"], merged.as_str());
    let done =
        wait_for(&s, "run-1-COM-01", "after_merge.commands").expect("the commands were recorded");
    assert_eq!(done.payload["ran"], true, "{}", done.payload);
    assert_eq!(
        git(&s.repo, &["show", "-s", "--format=%s", "HEAD"]),
        "chore(release): alpha 0.1.3"
    );
    // Another server start, and another round: nothing is done again.
    round(&s);
    round(&s);
    settle_time();
    assert_eq!(order(&s), ["after"]);
    assert_eq!(events_of(&s, "run-1-COM-01", "release.created").len(), 1);
    assert_eq!(
        events_of(&s, "run-1-COM-01", "after_merge.commands").len(),
        1
    );
    assert_eq!(*s.agent.asked.lock().unwrap(), 1);
}

#[test]
fn a_merge_a_release_commit_already_follows_is_recorded_not_ran_with_that_release() {
    let s = setup("unsettled-covered", RELEASING);
    let merged = merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    std::fs::write(
        s.repo.join("Cargo.toml"),
        "[workspace.package]\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    git(
        &s.repo,
        &["commit", "-q", "-am", "chore(release): alpha 0.2.0"],
    );
    round(&s);
    let done = wait_for(&s, "run-1-COM-01", "after_merge.commands").expect("recorded");
    assert_eq!(done.payload["ran"], false);
    assert_eq!(done.payload["reason"], "released by 0.2.0");
    assert_eq!(done.payload["merged"], merged.as_str());
    round(&s);
    settle_time();
    assert!(order(&s).is_empty());
    assert!(events_of(&s, "run-1-COM-01", "release.created").is_empty());
    assert_eq!(
        events_of(&s, "run-1-COM-01", "after_merge.commands").len(),
        1
    );
    assert_eq!(*s.agent.asked.lock().unwrap(), 0);
}

#[test]
fn the_server_counts_a_merge_as_unsettled_until_its_go_recorded_it() {
    let s = setup("unsettled-count", RELEASING);
    assert_eq!(provider(&s).merges_unsettled(), 0);
    merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    assert_eq!(provider(&s).merges_unsettled(), 1);
    round(&s);
    wait_for(&s, "run-1-COM-01", "after_merge.commands").expect("recorded");
    // The go lets go of the run just after it recorded it.
    let until = Instant::now() + Duration::from_secs(5);
    while provider(&s).merges_unsettled() != 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(provider(&s).merges_unsettled(), 0);
}

/// The Settings page as the browser shows it, with the server saying `server`; the texts it settled on.
fn settings_texts(server: Value) -> Option<Vec<String>> {
    if !web_deps::ready() {
        return None;
    }
    let reply = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}, "server": server}});
    let scenario = json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": {"/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]}},
        "responses": [reply], "steps": [], "selectors": ["[data-restart-cancel]"]
    });
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node")
        .arg(format!("{dir}/tests/web_react_harness.mjs"))
        .arg(format!("{dir}/web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let last = out["frames"].as_array().unwrap().last().unwrap().clone();
    Some(
        last["settled"]["texts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str())
            .map(String::from)
            .collect(),
    )
}

#[test]
fn the_settings_server_card_says_the_releases_a_pending_restart_waits_for() {
    let server = |runs: u64, releases: u64| json!({"running": "orq 0.28.0 (c0bc5e1 2026-11-25T11:00:00Z)", "installed": "orq 0.28.0 (c0bc5e1 2026-11-25T11:00:00Z)", "newer_installed": false, "restart_pending": true, "waiting_for": runs, "waiting_for_releases": releases});
    let Some(texts) = settings_texts(server(1, 1)) else {
        return;
    };
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Restart pending: waiting for 1 run, 1 release")),
        "{texts:?}"
    );
    let Some(texts) = settings_texts(server(0, 2)) else {
        return;
    };
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Restart pending: waiting for 2 releases")),
        "{texts:?}"
    );
    let Some(texts) = settings_texts(server(2, 0)) else {
        return;
    };
    assert!(
        texts
            .iter()
            .any(|t| t.ends_with("Restart pending: waiting for 2 runs")),
        "{texts:?}"
    );
}

#[test]
fn a_merge_whose_go_was_interrupted_before_its_release_gets_the_release_and_not_the_commands_again() {
    // The server stopped after the go started and before the release was made: the next round closed
    // the go as interrupted. The release is still owed; the commands are not run again.
    let s = setup("interrupted-release", RELEASING);
    let merged = merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    let store = RunStore::new(&s.roots.state);
    event(&store, "run-1-COM-01", "after_merge.started", json!({"merged": merged, "commands": 1}));
    event(&store, "run-1-COM-01", "after_merge.commands", json!({"ran": false, "ok": false, "interrupted": true, "merged": merged, "reason": "interrupted: the server stopped before the commands finished", "commands": []}));
    assert_eq!(provider(&s).merges_unsettled(), 1, "its release is owed");
    round(&s);
    let created = wait_for(&s, "run-1-COM-01", "release.created").expect("the release was made");
    assert_eq!(created.payload["version"], "0.1.3");
    let until = Instant::now() + Duration::from_secs(5);
    while provider(&s).merges_unsettled() != 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(provider(&s).merges_unsettled(), 0);
    round(&s);
    settle_time();
    assert!(order(&s).is_empty(), "the commands are not run again");
    assert_eq!(events_of(&s, "run-1-COM-01", "after_merge.commands").len(), 1);
    assert_eq!(events_of(&s, "run-1-COM-01", "release.created").len(), 1);
}

#[test]
fn a_merge_whose_after_merge_was_wrong_and_whose_release_never_ran_still_gets_the_release() {
    // A wrong [after_merge] is recorded on the run before the release runs; the server stopped in
    // between. The error says nothing about the release, which is still owed.
    let s = setup("wrong-after-merge-release", RELEASING);
    let merged = merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    let store = RunStore::new(&s.roots.state);
    event(&store, "run-1-COM-01", "after_merge.commands", json!({"ran": false, "ok": false, "merged": merged, "error": "the [after_merge] settings are wrong", "commands": []}));
    assert_eq!(provider(&s).merges_unsettled(), 1, "its release is owed");
    round(&s);
    let created = wait_for(&s, "run-1-COM-01", "release.created").expect("the release was made");
    assert_eq!(created.payload["version"], "0.1.3");
    let until = Instant::now() + Duration::from_secs(5);
    while provider(&s).merges_unsettled() != 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(provider(&s).merges_unsettled(), 0);
    round(&s);
    settle_time();
    assert!(order(&s).is_empty(), "the commands are not run");
    assert_eq!(events_of(&s, "run-1-COM-01", "after_merge.commands").len(), 1);
    assert_eq!(events_of(&s, "run-1-COM-01", "release.created").len(), 1);
}

#[test]
fn a_merge_in_a_project_whose_after_merge_has_no_commands_is_never_unsettled() {
    // Nothing to run and no release: the merge needs no go, so a pending restart never waits for it.
    let s = setup("unsettled-empty", "[after_merge]\ncommands = []\n");
    merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    assert_eq!(provider(&s).merges_unsettled(), 0);
    round(&s);
    settle_time();
    assert_eq!(provider(&s).merges_unsettled(), 0);
}

#[test]
fn a_merge_in_a_project_with_no_after_merge_or_release_is_never_unsettled() {
    let s = setup("unsettled-none", "");
    merged_unseen(&s, "run-1-COM-01", "feat: the thing");
    assert_eq!(provider(&s).merges_unsettled(), 0);
}
