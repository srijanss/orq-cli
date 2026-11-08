//! The projects the server lists: discovered under the scan root, each with counts read from the
//! same storage the CLI writes, and a project that cannot be read shown as unavailable.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::ProjectsProvider;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{RunRecord, RunStore};
use orq_remote::projects::StoreProjects;

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let status = Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap();
    assert!(status.success());
    path
}

fn roots(base: &Path) -> StorageRoots {
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn id_of(path: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(path).unwrap())
}

fn entry<'a>(list: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    list.as_array().unwrap().iter().find(|p| p["name"] == name).unwrap_or_else(|| panic!("no {name} in {list}"))
}

#[test]
fn each_project_lists_its_id_name_and_counts_and_an_unreadable_one_is_unavailable() {
    let base = web_deps::private_temp_dir("projects-provider");
    let scan = base.join("code");
    let busy = git_repo(&scan, "busy");
    git_repo(&scan, "quiet");
    let broken = git_repo(&scan, "broken");
    std::fs::write(busy.join(".tdd-features.json"), r#"[{"featureName":"a","status":"pending"},{"featureName":"b","status":"pending"}]"#).unwrap();
    std::fs::write(broken.join(".tdd-features.json"), "{ not json").unwrap();
    let roots = roots(&base);
    let busy_id = id_of(&busy);

    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", &busy_id, "a", &"a".repeat(40), b"{}").unwrap();
    store.save_review(&roots.data, "run-2", "someone-else", "x", &"b".repeat(40), b"{}").unwrap();
    let runs = RunStore::new(&roots.state);
    let record = |run: &str, project: &str, feature: &str| RunRecord {
        run_id: run.into(),
        project_id: project.into(),
        feature_id: feature.into(),
        remote: String::new(),
        provider: String::new(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: format!("orq/{run}"),
        worktree: base.join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    runs.create(&record("r1", &busy_id, "a")).unwrap();
    runs.create(&record("r2", "someone-else", "x")).unwrap();

    let list = StoreProjects::new(roots, scan).projects().unwrap();
    let busy = entry(&list, "busy");
    assert_eq!(busy["id"], "busy");
    assert_eq!(busy["available"], true);
    assert_eq!((busy["features"].as_u64(), busy["active_runs"].as_u64(), busy["reviews_waiting"].as_u64()), (Some(2), Some(1), Some(1)));
    let quiet = entry(&list, "quiet");
    assert_eq!((quiet["features"].as_u64(), quiet["active_runs"].as_u64(), quiet["reviews_waiting"].as_u64()), (Some(0), Some(0), Some(0)));
    let broken = entry(&list, "broken");
    assert_eq!(broken["available"], false);
    assert!(broken["features"].is_null(), "{broken}");
    assert_eq!(list.as_array().unwrap().len(), 3);
}

#[test]
fn a_run_record_that_cannot_be_read_makes_the_counts_untrustworthy_so_projects_are_unavailable() {
    let base = web_deps::private_temp_dir("projects-corrupt-run");
    let scan = base.join("code");
    git_repo(&scan, "one");
    let roots = roots(&base);
    let corrupt = roots.state.join("runs").join("bad-run");
    std::fs::create_dir_all(&corrupt).unwrap();
    std::fs::write(corrupt.join("run.json"), "{ not json").unwrap();
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    assert_eq!(entry(&list, "one")["available"], false, "{list}");
}

#[test]
fn only_reviews_still_awaiting_a_human_count_as_waiting() {
    use orq_core::review_lifecycle::{Gate, ProviderState, ReviewLifecycle};
    let base = web_deps::private_temp_dir("projects-waiting");
    let scan = base.join("code");
    let repo = git_repo(&scan, "reviewed");
    let roots = roots(&base);
    let id = id_of(&repo);
    let store = Store::open(&roots.database()).unwrap();
    let lifecycle = |head: &str, state, gate| ReviewLifecycle { provider: "github".into(), review_id: "1".into(), url: "https://github.com/o/r/pull/1".into(), reported_head_sha: head.into(), observed_head_sha: None, state, gate, stale: false, last_error: None };
    for (run, head, lifecycle_to_save) in [
        ("waiting-none", 'a', None),
        ("waiting-open", 'b', Some(lifecycle(&"b".repeat(40), ProviderState::Open, Gate::AwaitingHumanReview))),
        ("done-merged", 'c', Some(lifecycle(&"c".repeat(40), ProviderState::Merged, Gate::Resolved))),
        ("done-closed", 'd', Some(lifecycle(&"d".repeat(40), ProviderState::Closed, Gate::Resolved))),
        ("done-resolved", 'e', Some(lifecycle(&"e".repeat(40), ProviderState::Open, Gate::Resolved))),
    ] {
        store.save_review(&roots.data, run, &id, "f", &head.to_string().repeat(40), b"{}").unwrap();
        if let Some(l) = lifecycle_to_save {
            store.save_lifecycle(run, &l).unwrap();
        }
    }
    // A second, newer report for a finished run is the one that counts, not the older one.
    store.save_review(&roots.data, "done-merged", &id, "f", &"f".repeat(40), b"{}").unwrap();
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    assert_eq!(entry(&list, "reviewed")["reviews_waiting"], 3, "{list}");
}

#[test]
fn every_project_gets_only_its_own_active_runs_when_several_projects_have_runs() {
    let base = web_deps::private_temp_dir("projects-own-runs");
    let scan = base.join("code");
    let (one, two) = (git_repo(&scan, "one"), git_repo(&scan, "two"));
    let roots = roots(&base);
    let runs = RunStore::new(&roots.state);
    let mut n = 0;
    for (project, count) in [(id_of(&one), 2), (id_of(&two), 3)] {
        for _ in 0..count {
            n += 1;
            let run = format!("run-{n}");
            runs.create(&RunRecord { run_id: run.clone(), project_id: project.clone(), feature_id: format!("f{n}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: base.join(&run), repository: String::new(), profiles: Default::default() }).unwrap();
        }
    }
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    assert_eq!((entry(&list, "one")["active_runs"].as_u64(), entry(&list, "two")["active_runs"].as_u64()), (Some(2), Some(3)), "{list}");
}

#[test]
fn a_ledger_that_is_a_link_a_pipe_or_far_too_large_makes_its_project_unavailable_without_being_followed() {
    let base = web_deps::private_temp_dir("projects-ledger");
    let scan = base.join("code");
    let linked = git_repo(&scan, "linked");
    let piped = git_repo(&scan, "piped");
    let huge = git_repo(&scan, "huge");
    let plain = git_repo(&scan, "plain");
    let secret = base.join("outside.json");
    std::fs::write(&secret, r#"[{"featureName":"a","status":"pending"}]"#).unwrap();
    std::os::unix::fs::symlink(&secret, linked.join(".tdd-features.json")).unwrap();
    assert!(Command::new("mkfifo").arg(piped.join(".tdd-features.json")).status().unwrap().success());
    std::fs::write(huge.join(".tdd-features.json"), vec![b' '; 8 * 1024 * 1024]).unwrap();
    std::fs::write(plain.join(".tdd-features.json"), r#"[{"featureName":"a","status":"pending"}]"#).unwrap();
    // A read that blocks on the pipe must fail the test, not hang it.
    let (sender, receiver) = std::sync::mpsc::channel();
    let roots = roots(&base);
    std::thread::spawn(move || {
        let _ = sender.send(StoreProjects::new(roots, scan).projects());
    });
    let list = receiver.recv_timeout(std::time::Duration::from_secs(20)).expect("listing the projects blocked").unwrap();
    for name in ["linked", "piped", "huge"] {
        assert_eq!(entry(&list, name)["available"], false, "{name}: {list}");
    }
    assert_eq!(entry(&list, "plain")["features"], 1, "{list}");
}

fn run_record(base: &Path, run: &str, project: &str) -> RunRecord {
    RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: base.join(run), repository: String::new(), profiles: Default::default() }
}

fn event(run: &str, project: &str, n: u32, kind: &str, payload: serde_json::Value) -> orq_orchestrator::Event {
    orq_orchestrator::Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: kind.into(), stage: None, attempt: 1, occurred_at: "2026-11-08T00:00:00Z".into(), payload }
}

#[test]
fn only_runs_that_are_running_or_pausing_count_as_active_and_a_bad_event_log_makes_projects_unavailable() {
    let base = web_deps::private_temp_dir("projects-events");
    let scan = base.join("code");
    let repo = git_repo(&scan, "evented");
    let roots = roots(&base);
    let id = id_of(&repo);
    let runs = RunStore::new(&roots.state);
    let histories: [(&str, Vec<(&str, serde_json::Value)>); 4] = [
        ("r-running", vec![]),
        ("r-paused", vec![("run.pause_requested", serde_json::json!({})), ("run.paused", serde_json::json!({}))]),
        ("r-done", vec![("run.completed", serde_json::json!({"ending": "review_ready"}))]),
        ("r-failed", vec![("run.failed", serde_json::json!({"reason": "x"}))]),
    ];
    for (run, history) in &histories {
        runs.create(&run_record(&base, run, &id)).unwrap();
        for (n, (kind, payload)) in history.iter().enumerate() {
            runs.append_event(&event(run, &id, n as u32, kind, payload.clone())).unwrap();
        }
    }
    let list = StoreProjects::new(roots.clone(), scan.clone()).projects().unwrap();
    assert_eq!(entry(&list, "evented")["active_runs"], 2, "{list}");

    std::fs::write(roots.state.join("runs").join("r-running").join("events.jsonl"), "not an event\n").unwrap();
    let list = StoreProjects::new(roots, scan).projects().unwrap();
    assert_eq!(entry(&list, "evented")["available"], false, "{list}");
    assert!(entry(&list, "evented")["active_runs"].is_null(), "{list}");
}
