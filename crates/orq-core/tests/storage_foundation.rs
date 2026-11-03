use std::collections::HashMap;
use std::path::PathBuf;

use orq_core::storage::{save_run_file, RunFile, StorageRoots};
use orq_core::store::Store;

const SHA: &str = "d8d5295000000000000000000000000000000001";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-foundation-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn orq_data_lives_outside_the_project_and_is_readable_by_a_restarted_orq() {
    let home = scratch("home");
    let project = scratch("project");
    let env_home = home.to_str().unwrap().to_string();
    let env = move |name: &str| -> Option<String> {
        HashMap::from([("HOME".to_string(), env_home.clone())]).get(name).cloned()
    };

    // First run: create the roots, store a report and the run's files.
    let roots = StorageRoots::resolve(&env).unwrap();
    roots.create_all().unwrap();
    {
        let store = Store::open(&roots.database()).unwrap();
        store.save_review(&roots.data, "run1", "alpha", "login", SHA, b"{\"findings\":[]}").unwrap();
        save_run_file(&roots.data, "run1", RunFile::Handoff, b"{}").unwrap();
        save_run_file(&roots.data, "run1", RunFile::Events, b"{\"e\":1}\n").unwrap();
    }

    // Nothing was written into the project tree.
    assert_eq!(std::fs::read_dir(&project).unwrap().count(), 0);
    assert!(roots.data.starts_with(&home) && !roots.data.starts_with(&project));

    // A restarted Orq resolves the same roots and finds the report by every key.
    let roots = StorageRoots::resolve(&env).unwrap();
    let store = Store::open(&roots.database()).unwrap();
    for found in [
        store.reviews_for_run("run1").unwrap(),
        store.reviews_for_project("alpha").unwrap(),
        store.reviews_for_feature("alpha", "login").unwrap(),
        store.reviews_for_head_sha(SHA).unwrap(),
    ] {
        assert_eq!(found.len(), 1);
        assert_eq!(std::fs::read(roots.data.join(&found[0].report_path)).unwrap(), b"{\"findings\":[]}");
    }
    assert_eq!(std::fs::read(roots.runs().join("run1/handoff.json")).unwrap(), b"{}");
}
