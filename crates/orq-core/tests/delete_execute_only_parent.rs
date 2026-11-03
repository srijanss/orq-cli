#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use orq_core::ownership::delete_in_root;

/// Puts the directory back so the test's temp files can be cleaned up.
struct Restore<'a>(&'a Path);
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_root_under_a_search_only_parent_can_still_be_deleted_from() {
    let base = std::env::temp_dir().join(format!("orq-execonly-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let parent = base.join("parent");
    let root = parent.join("orq");
    std::fs::create_dir_all(root.join("runs/r1")).unwrap();
    std::fs::write(root.join("runs/r1/events.jsonl"), "x").unwrap();
    // Search (x) without read (r): the root is reachable by name but the parent cannot be opened for reading.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o111)).unwrap();
    let _restore = Restore(&parent);
    if std::fs::read_dir(&parent).is_ok() {
        return; // running with privileges that ignore directory permissions
    }
    delete_in_root(&root, Path::new("runs/r1/events.jsonl")).unwrap();
    assert!(!root.join("runs/r1/events.jsonl").exists());
}
