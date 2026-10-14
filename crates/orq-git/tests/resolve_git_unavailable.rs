use std::os::unix::fs::PermissionsExt;

use orq_git::{resolve_project, ResolveError};

#[test]
fn missing_or_failing_git_is_a_typed_error_carrying_the_path() {
    let base = std::env::temp_dir().join(format!("orq-git-unavailable-{}", std::process::id()));
    let workdir = base.join("work");
    let empty_bin = base.join("empty-bin");
    let failing_bin = base.join("failing-bin");
    for dir in [&workdir, &empty_bin, &failing_bin] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let fake_git = failing_bin.join("git");
    std::fs::write(&fake_git, "#!/bin/sh\necho 'boom: disk on fire' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();

    std::env::set_var("PATH", &empty_bin);
    let err = resolve_project(&workdir).unwrap_err();
    assert!(
        matches!(&err, ResolveError::GitUnavailable { path, .. } if *path == workdir),
        "git missing: {err:?}"
    );

    std::env::set_var("PATH", &failing_bin);
    let err = resolve_project(&workdir).unwrap_err();
    match err {
        ResolveError::GitUnavailable { path, message } => {
            assert_eq!(path, workdir);
            assert!(message.contains("boom"), "{message}");
        }
        other => panic!("expected GitUnavailable, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&base);
}
