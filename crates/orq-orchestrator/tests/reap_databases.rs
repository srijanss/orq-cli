use std::fs;
use std::os::unix::fs::PermissionsExt;

use orq_orchestrator::{load_run_settings, reap_run_databases};

/// Scripts written in one test must not be inherited open by another test's fork (ETXTBSY).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo(tag: &str, toml: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-reap-db-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".orq")).unwrap();
    fs::write(
        dir.join(".env"),
        "DB_NAME=shop\nDATABASE_URL=postgres://u:pw@h/shop_api\n",
    )
    .unwrap();
    let log = dir.join("drops.log");
    let tool = dir.join("drop-tool");
    fs::write(
        &tool,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        dir.join(".orq/run.toml"),
        toml.replace("TOOL", &tool.display().to_string()),
    )
    .unwrap();
    (dir, log)
}

#[test]
fn a_reaped_runs_test_databases_are_dropped_by_the_names_its_branch_gave_them() {
    let _serial = serial();
    let (dir, log) = repo(
        "drop",
        "test_db_vars = [\"DB_NAME\", \"DATABASE_URL\"]\ntest_db_drop_cmd = [\"TOOL\"]\n",
    );
    let settings = load_run_settings(&dir, &|_| None).unwrap();

    let notes = reap_run_databases(&dir, "fix--COM-01", &settings);

    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, ["test_shop_fix_com_01", "test_shop_api_fix_com_01"]);
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_missing_drop_command_and_a_failing_one_are_notes_that_name_the_test_database() {
    let _serial = serial();
    let (dir, _) = repo("nocmd", "test_db_vars = [\"DB_NAME\"]\n");
    let settings = load_run_settings(&dir, &|_| None).unwrap();
    let notes = reap_run_databases(&dir, "fix--COM-01", &settings);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("test_shop_fix_com_01") && n.contains("manually")),
        "{notes:?}"
    );

    let (dir, _) = repo(
        "failing",
        "test_db_vars = [\"DB_NAME\"]\ntest_db_drop_cmd = [\"/nonexistent/drop-tool\"]\n",
    );
    let settings = load_run_settings(&dir, &|_| None).unwrap();
    let notes = reap_run_databases(&dir, "fix--COM-01", &settings);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("test_shop_fix_com_01") && n.contains("manually")),
        "{notes:?}"
    );
}

#[test]
fn without_a_drop_command_or_with_keep_test_db_the_names_are_reported_not_dropped() {
    let _serial = serial();
    let (dir, log) = repo(
        "keep",
        "test_db_vars = [\"DB_NAME\"]\nkeep_test_db = true\ntest_db_drop_cmd = [\"TOOL\"]\n",
    );
    let settings = load_run_settings(&dir, &|_| None).unwrap();

    let notes = reap_run_databases(&dir, "fix--COM-01", &settings);

    assert!(!log.exists());
    assert!(
        notes
            .iter()
            .any(|n| n.contains("test_shop_fix_com_01") && n.contains("kept")),
        "{notes:?}"
    );
}
