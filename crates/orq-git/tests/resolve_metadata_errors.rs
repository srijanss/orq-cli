use std::os::unix::fs::PermissionsExt;

use orq_git::{resolve_project, ResolveError};

#[test]
fn an_unreadable_parent_is_git_unavailable_not_not_a_repository() {
    let base = std::env::temp_dir().join(format!("orq-metadata-{}", std::process::id()));
    let locked = base.join("locked");
    let inner = locked.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    // A privileged user (e.g. root in CI containers) ignores mode 000, so the
    // scenario cannot be produced there: skip rather than fail spuriously.
    if std::fs::metadata(&inner).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        eprintln!("skipped: permission restrictions are not enforced for this user");
        return;
    }

    let result = resolve_project(&inner);

    // Restore access so cleanup works, even if the assertion below fails.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&base);

    match result.unwrap_err() {
        ResolveError::GitUnavailable { path, message } => {
            assert_eq!(path, inner);
            assert!(!message.is_empty());
        }
        other => panic!("expected GitUnavailable, got {other:?}"),
    }
}
