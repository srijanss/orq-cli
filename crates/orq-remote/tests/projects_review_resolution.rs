//! Reviews of runs merged locally before merges resolved them are backfilled the first time the
//! server reads its projects, so they stop counting as waiting; and a person's action on a review from
//! the web is recorded in the run's events.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::ProjectsProvider;
use orq_core::review_inbox::ReviewAction;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MERGED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

fn record(base: &Path, run: &str, project: &str) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: project.into(),
        feature_id: format!("feat-{run}"),
        remote: String::new(),
        provider: String::new(),
        target_branch: "main".into(),
        base_sha: "c".repeat(40),
        branch: format!("orq/{run}"),
        worktree: base.join(run),
        repository: String::new(),
        profiles: Default::default(),
    }
}

/// A project with `merged` (merged locally at MERGED) and `open` (not merged), both reviewed at HEAD.
fn fixture(tag: &str) -> (StorageRoots, PathBuf, String) {
    let base = web_deps::private_temp_dir(tag);
    let scan = base.join("code");
    let repo = git_repo(&scan, "proj");
    let roots = roots(&base);
    let id = orq_git::project_id(&orq_git::resolve_project(&repo).unwrap());
    let store = Store::open(&roots.database()).unwrap();
    let runs = RunStore::new(&roots.state);
    for run in ["merged", "open"] {
        store.save_review(&roots.data, run, &id, "f", HEAD, br#"{"status":"current","risks":[]}"#).unwrap();
        runs.create(&record(&base, run, &id)).unwrap();
    }
    runs.append_event(&Event {
        schema_version: 1,
        event_id: "merged-1".into(),
        run_id: "merged".into(),
        project_id: id.clone(),
        feature_id: "feat-merged".into(),
        kind: "merge.completed".into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-01T10:00:00Z".into(),
        payload: serde_json::json!({"head_sha": MERGED, "target_branch": "main"}),
    })
    .unwrap();
    (roots, scan, id)
}

#[test]
fn a_run_merged_locally_before_this_change_stops_waiting_once_the_projects_are_read() {
    let (roots, scan, id) = fixture("projects-backfill");
    let provider = StoreProjects::new(roots.clone(), scan);

    let list = provider.projects().unwrap();
    assert_eq!(list[0]["reviews_waiting"].as_u64(), Some(1), "only the unmerged run waits: {list}");

    let rows = orq_core::review_inbox::inbox_of_project(&roots, &id, "").unwrap();
    let status = |run: &str| rows.iter().find(|row| row.run_id == run).map(|row| row.status.clone());
    assert_eq!((status("merged").as_deref(), status("open").as_deref()), (Some("merged"), Some("ready")));
}

#[test]
fn an_action_from_the_web_is_recorded_in_the_runs_events() {
    let (roots, scan, id) = fixture("projects-review-acted");
    let provider = StoreProjects::new(roots.clone(), scan);

    provider.review_acted(&id, "open", ReviewAction::Dismiss, HEAD).unwrap();

    let events = RunStore::new(&roots.state).events("open").unwrap();
    let event = events.iter().find(|e| e.kind == "review.dismissed").expect("a review.dismissed event");
    assert_eq!(event.payload, serde_json::json!({"head_sha": HEAD, "by": "web"}));
    assert!(provider.review_acted(&id, "gone", ReviewAction::Resolve, HEAD).is_err(), "an unknown run is an error for the server log");
}
