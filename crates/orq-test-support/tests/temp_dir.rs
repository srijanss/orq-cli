use std::os::unix::fs::PermissionsExt;

use orq_test_support::TempDir;

#[test]
fn a_temp_dir_is_private_distinct_and_removed_when_dropped() {
    let first = TempDir::new("orq-temp-dir-test");
    let second = TempDir::new("orq-temp-dir-test");
    assert_ne!(first.path(), second.path());
    assert_eq!(std::fs::metadata(first.path()).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(first.path().starts_with(std::env::temp_dir()));

    // It derefs to its path, and what is inside goes with it.
    std::fs::write(first.join("file"), "x").unwrap();
    std::fs::create_dir(first.join("nested")).unwrap();
    let (a, b) = (first.path().to_path_buf(), second.path().to_path_buf());
    drop(first);
    assert!(!a.exists(), "{a:?} was left behind");
    assert!(b.exists());
    drop(second);
    assert!(!b.exists());
}

#[test]
fn a_temp_dir_is_removed_when_the_test_panics() {
    let path = std::panic::catch_unwind(|| {
        let dir = TempDir::new("orq-temp-dir-panic");
        let path = dir.path().to_path_buf();
        std::fs::write(dir.join("file"), "x").unwrap();
        std::panic::resume_unwind(Box::new(path));
    })
    .unwrap_err()
    .downcast::<std::path::PathBuf>()
    .unwrap();
    assert!(!path.exists(), "{path:?} was left behind");
}
