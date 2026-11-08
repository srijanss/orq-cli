//! The inbox of one project holds only that project's rows, chosen before any report is opened,
//! so a store full of other projects' reports costs nothing to a project that has none.

use orq_core::review_inbox::{inbox, inbox_of_project};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-project-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

#[test]
fn a_project_inbox_has_only_its_own_rows_and_another_projects_unreadable_report_is_never_opened() {
    let roots = roots("rows");
    let store = Store::open(&roots.database()).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({"status": "current", "summary": {}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []})).unwrap();
    store.save_review(&roots.data, "run-a", "alpha-00000001", "fa", &"a".repeat(40), &body).unwrap();
    let other = store.save_review(&roots.data, "run-b", "beta-00000002", "fb", &"b".repeat(40), &body).unwrap();
    std::fs::remove_file(roots.data.join(&other.report_path)).unwrap();

    let rows = inbox_of_project(&roots, "alpha-00000001", "alpha").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].run_id.as_str(), rows[0].project.as_str(), rows[0].current, rows[0].status.as_str()), ("run-a", "alpha", true, "ready"));
    // The whole inbox still lists both, and shows the missing report of the other one as unreadable.
    let all = inbox(&roots, "alpha-00000001", "alpha").unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all.iter().find(|r| r.run_id == "run-b").unwrap().status, "unreadable");
    assert!(inbox_of_project(&roots, "gamma-00000003", "gamma").unwrap().is_empty());
}
