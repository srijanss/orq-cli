use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use orq_orchestrator::{drop_test_databases, drop_test_databases_within};

/// The tests write executable scripts and spawn them; a script still open for writing in one test
/// would be inherited by another test's fork and refuse to execute (ETXTBSY), so they run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A stand-in drop tool that logs its arguments and fails for any name containing "bad".
fn fake_tool(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("drops.log");
    let tool = dir.join("fake-drop");
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$PWD $*\" >> '{}'\ncase \"$*\" in *bad*) exit 3;; esac\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    (tool, log)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-drop-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn each_per_worktree_test_database_is_dropped_through_the_configured_command_from_the_main_checkout(
) {
    let _serial = serial();
    let dir = scratch("drop");
    let (tool, log) = fake_tool(&dir);
    let command = vec![tool.display().to_string(), "--drop".to_string()];

    let notes = drop_test_databases(
        &dir,
        &pairs(&[("DB_NAME", "shop_fix_a"), ("B2C_DB_NAME", "b2c_fix_a")]),
        Some(&command),
        false,
    );

    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].ends_with("--drop test_shop_fix_a"), "{lines:?}");
    assert!(lines[1].ends_with("--drop test_b2c_fix_a"), "{lines:?}");
    assert!(
        lines[0].starts_with(&dir.canonicalize().unwrap().display().to_string()),
        "run from the main checkout: {lines:?}"
    );
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn keeping_no_command_an_unsafe_name_and_a_failing_command_are_notes_never_failures() {
    let _serial = serial();
    let dir = scratch("notes");
    let (tool, log) = fake_tool(&dir);
    let command = vec![tool.display().to_string()];
    let names = pairs(&[("DB_NAME", "shop_fix_a")]);

    let kept = drop_test_databases(&dir, &names, Some(&command), true);
    assert!(
        kept.iter()
            .any(|n| n.contains("test_shop_fix_a") && n.contains("KEEP_TEST_DB")),
        "{kept:?}"
    );
    assert!(
        !log.exists(),
        "nothing is dropped when the databases are kept"
    );

    let none = drop_test_databases(&dir, &names, None, false);
    assert!(
        none.iter()
            .any(|n| n.contains("test_shop_fix_a") && n.contains("manually")),
        "{none:?}"
    );

    let unsafe_names = drop_test_databases(
        &dir,
        &pairs(&[("DB_NAME", "a`b"), ("X", "ok_name")]),
        Some(&command),
        false,
    );
    assert!(
        unsafe_names
            .iter()
            .any(|n| n.contains("DB_NAME") && n.contains("not dropped")),
        "{unsafe_names:?}"
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "only the safe name ran"
    );

    let failing = drop_test_databases(
        &dir,
        &pairs(&[("DB_NAME", "bad_one")]),
        Some(&command),
        false,
    );
    assert!(
        failing
            .iter()
            .any(|n| n.contains("test_bad_one") && n.contains("manually")),
        "{failing:?}"
    );

    let missing = drop_test_databases(
        &dir,
        &names,
        Some(&["/nonexistent/drop-tool".to_string()]),
        false,
    );
    assert!(
        missing.iter().any(|n| n.contains("test_shop_fix_a")),
        "{missing:?}"
    );

    assert!(
        drop_test_databases(&dir, &[], Some(&command), false).is_empty(),
        "no databases, nothing said"
    );
}

#[test]
fn a_drop_command_that_hangs_is_stopped_and_becomes_a_manual_cleanup_note() {
    let _serial = serial();
    let dir = scratch("hang");
    let tool = dir.join("hang-drop");
    fs::write(&tool, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();

    let notes = drop_test_databases_within(
        &dir,
        &pairs(&[("DB_NAME", "shop_fix_a")]),
        Some(&[tool.display().to_string()]),
        false,
        std::time::Duration::from_millis(300),
    );

    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "must not wait for the command"
    );
    assert!(
        notes.iter().any(|n| n.contains("test_shop_fix_a")
            && n.contains("timed out")
            && n.contains("manually")),
        "{notes:?}"
    );
}

#[test]
fn a_database_named_by_two_variables_is_dropped_once() {
    let _serial = serial();
    let dir = scratch("dedupe");
    let (tool, log) = fake_tool(&dir);

    let notes = drop_test_databases(
        &dir,
        &pairs(&[("DB_NAME", "shop_fix_a"), ("READ_DB_NAME", "shop_fix_a")]),
        Some(&[tool.display().to_string()]),
        false,
    );

    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "one drop for one database"
    );
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_timed_out_drop_command_takes_the_processes_it_started_down_with_it() {
    let _serial = serial();
    let dir = scratch("group");
    let marker = dir.join("late-marker");
    let tool = dir.join("spawning-drop");
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\n(sleep 2; echo late > '{}') &\nsleep 30\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();

    let notes = drop_test_databases_within(
        &dir,
        &pairs(&[("DB_NAME", "shop_fix_a")]),
        Some(&[tool.display().to_string()]),
        false,
        std::time::Duration::from_millis(300),
    );

    assert!(notes.iter().any(|n| n.contains("timed out")), "{notes:?}");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(
        !marker.exists(),
        "a descendant of the drop command outlived its timeout"
    );
}
