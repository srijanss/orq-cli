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
