#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{collect_run_facts, RunRecord, RunStore};

struct Restore<'a>(&'a Path);
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_worktree_that_cannot_be_inspected_is_treated_as_in_use() {
    let base =
        orq_test_support::process_temp_dir().join(format!("orq-wt-err-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    let runs = RunStore::new(&base.join("runs-state"));
    let locked = base.join("locked");
    std::fs::create_dir_all(locked.join("worktree")).unwrap();
    runs.create(&RunRecord {
        delivery: Default::default(),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "abc".into(),
        branch: "b".into(),
        worktree: locked.join("worktree"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    })
    .unwrap();
    // No search permission on the parent: the worktree can be neither seen nor ruled out.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = Restore(&locked);
    if std::fs::symlink_metadata(locked.join("worktree")).is_ok() {
        return; // running with privileges that ignore directory permissions
    }
    let all = collect_run_facts(&runs, &store).unwrap();
    assert!(
        all["run-1"].worktree_in_use,
        "only NotFound proves the worktree is gone"
    );
}
