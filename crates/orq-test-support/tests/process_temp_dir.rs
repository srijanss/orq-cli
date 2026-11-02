use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use orq_test_support::process_temp_dir;

const CHILD: &str = "ORQ_PROCESS_TEMP_DIR_CHILD";

/// Run only as the child of the test below: makes a mess in the process temp dir and reports where.
#[test]
fn child_leaves_a_mess() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let dir = process_temp_dir();
    assert_eq!(dir, process_temp_dir(), "one directory for the whole process");
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    std::fs::write(dir.join("a/b/file"), "x").unwrap();
    println!("dir={}", dir.display());
}

#[test]
fn the_process_temp_dir_is_private_inside_the_temp_dir_and_gone_when_the_process_exits() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["child_leaves_a_mess", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let dir = PathBuf::from(stdout.lines().find_map(|l| l.split_once("dir=").map(|(_, dir)| dir)).expect(&stdout));
    assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
    assert!(!dir.exists(), "{dir:?} was left behind");

    // While alive, it exists, is private, and is the same directory every time.
    let mine = process_temp_dir();
    assert!(mine.is_dir());
    assert_eq!(std::fs::metadata(&mine).unwrap().permissions().mode() & 0o777, 0o700);
    assert_ne!(mine, dir);
}
