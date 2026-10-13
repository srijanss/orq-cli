use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

use orq_git::resolve_project;

#[test]
fn repository_paths_with_newlines_and_non_utf8_bytes_are_preserved() {
    let base = std::env::temp_dir().join(format!("orq-unusual-{}", std::process::id()));
    let weird = OsString::from_vec(b"re\npo-\xff".to_vec());
    let root = base.join(weird);
    let nested = root.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    let status = Command::new("git").arg("init").arg("--quiet").arg(&root).status().unwrap();
    assert!(status.success());

    let project = resolve_project(&nested).unwrap();

    let expected_root = root.canonicalize().unwrap();
    assert_eq!(project.root, expected_root);
    assert_eq!(project.git_common_dir, expected_root.join(".git"));
    let _ = std::fs::remove_dir_all(&base);
}
