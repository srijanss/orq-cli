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
