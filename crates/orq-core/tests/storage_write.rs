use std::path::{Path, PathBuf};

use orq_core::storage::write_atomic;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-storage-write-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_write_creates_parents_and_leaves_an_owner_only_file_with_no_temporaries() {
    let root = scratch("basic");
    write_atomic(&root, Path::new("runs/r1/review/abc/review.json"), b"{\"v\":1}").unwrap();
    write_atomic(&root, Path::new("runs/r1/review/abc/review.json"), b"{\"v\":2}").unwrap();
    let file = root.join("runs/r1/review/abc/review.json");
    assert_eq!(std::fs::read(&file).unwrap(), b"{\"v\":2}");
    let names: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("review.json")], "no temporary file may remain");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(file.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

#[test]
fn paths_that_escape_the_root_are_refused() {
    let root = scratch("escape");
    for bad in ["../outside.txt", "/abs/outside.txt", "a/../../outside.txt", "", "."] {
        assert!(write_atomic(&root, Path::new(bad), b"x").is_err(), "{bad:?} must be refused");
    }
    assert!(!root.parent().unwrap().join("outside.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_never_followed_and_a_symlinked_target_is_replaced_not_written_through() {
    let root = scratch("symlink");
    let elsewhere = scratch("symlink-elsewhere");
    std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
    assert!(write_atomic(&root, Path::new("link/file.txt"), b"x").is_err());
    assert!(!elsewhere.join("file.txt").exists());

    let victim = elsewhere.join("victim.txt");
    std::fs::write(&victim, b"keep").unwrap();
    std::os::unix::fs::symlink(&victim, root.join("target.txt")).unwrap();
    write_atomic(&root, Path::new("target.txt"), b"new").unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep", "the symlink's target must be untouched");
    assert_eq!(std::fs::read(root.join("target.txt")).unwrap(), b"new");
}

#[test]
fn concurrent_writers_never_leave_a_partial_file() {
    let root = scratch("concurrent");
    let payloads: Vec<Vec<u8>> = (0..8u8).map(|n| vec![b'a' + n; 64 * 1024]).collect();
    std::thread::scope(|scope| {
        for payload in &payloads {
            let root = &root;
            scope.spawn(move || {
                for _ in 0..20 {
                    write_atomic(root, Path::new("shared/report.json"), payload).unwrap();
                }
            });
        }
    });
    let content = std::fs::read(root.join("shared/report.json")).unwrap();
    assert!(payloads.contains(&content), "the file must be exactly one writer's payload");
    assert_eq!(std::fs::read_dir(root.join("shared")).unwrap().count(), 1, "no temporary file may remain");
}
