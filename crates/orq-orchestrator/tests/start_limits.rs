//! The two refusals a project's settings can make of a run start: too many active runs, or too
//! little free disk on the filesystem the run's worktree would be made on. Each names what it
//! found and what to do about it.

use std::path::{Path, PathBuf};

use orq_orchestrator::{disk_refusal, free_disk, limit_refusal, FreeDisk};

fn active(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(run, feature)| (run.to_string(), feature.to_string())).collect()
}

#[test]
fn a_start_at_the_limit_names_the_active_runs_and_under_it_or_without_one_is_not_refused() {
    let one = active(&[("run-a", "COM-02")]);
    assert_eq!(
        limit_refusal("orq-cli", Some(1), &one).as_deref(),
        Some("orq-cli already has 1 active run (run-a, COM-02): wait for it to finish, or raise the limit in project settings"),
    );
    let two = active(&[("run-a", "COM-02"), ("run-b", "COM-03")]);
    assert_eq!(
        limit_refusal("orq-cli", Some(2), &two).as_deref(),
        Some("orq-cli already has 2 active runs (run-a, COM-02; run-b, COM-03): wait for one to finish, or raise the limit in project settings"),
    );
    assert_eq!(limit_refusal("orq-cli", Some(3), &two), None, "under the limit");
    assert_eq!(limit_refusal("orq-cli", None, &two), None, "no limit");
}

#[test]
fn a_start_under_the_disk_floor_names_what_is_free_where_and_what_is_needed() {
    let free = |bytes: u64| FreeDisk { bytes, mount: PathBuf::from("/") };
    assert_eq!(
        disk_refusal("orq-cli", Some(15), &free(9_800_000_000)).as_deref(),
        Some("only 9.8 GB free on / (orq-cli needs 15 GB to start a run): free some space, or lower the floor in project settings"),
    );
    assert_eq!(disk_refusal("orq-cli", Some(15), &free(15_000_000_000)), None, "exactly the floor is enough");
    assert_eq!(disk_refusal("orq-cli", None, &free(0)), None, "no floor");
}

#[test]
fn free_disk_is_measured_on_the_filesystem_holding_the_path_and_a_debug_build_takes_an_override() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-start-limits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary that reads or writes the variable.
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };
    let measured = free_disk(&dir).unwrap();
    assert!(dir.starts_with(&measured.mount), "{} holds {}", measured.mount.display(), dir.display());
    let device = |path: &Path| std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(path).unwrap());
    assert_eq!(device(&measured.mount), device(&dir), "the same filesystem");
    if let Some(parent) = measured.mount.parent() {
        assert_ne!(device(parent), device(&measured.mount), "its mount point, not a directory inside it");
    }

    unsafe { std::env::set_var("ORQ_TEST_FREE_DISK_BYTES", "9800000000") };
    let overridden = free_disk(&dir).unwrap();
    unsafe { std::env::remove_var("ORQ_TEST_FREE_DISK_BYTES") };
    assert_eq!((overridden.bytes, overridden.mount), (9_800_000_000, measured.mount), "tests build in debug");
}
