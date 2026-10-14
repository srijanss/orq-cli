use orq_git::{resolve_project, ResolveError};

#[test]
fn paths_outside_any_repository_are_a_typed_error_carrying_the_path() {
    let base = std::env::temp_dir().join(format!("orq-not-a-repo-{}", std::process::id()));
    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    // Never walk up into whatever repository might contain the temp dir.
    std::env::set_var("GIT_CEILING_DIRECTORIES", &base);

    let err = resolve_project(&plain).unwrap_err();
    assert_eq!(err, ResolveError::NotARepository { path: plain.clone() });

    let missing = base.join("does-not-exist");
    let err = resolve_project(&missing).unwrap_err();
    assert_eq!(err, ResolveError::NotARepository { path: missing });

    let _ = std::fs::remove_dir_all(&base);
}
