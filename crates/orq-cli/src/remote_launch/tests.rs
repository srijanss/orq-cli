//! The web server's launcher asks `orq run --check` first, then starts the run detached, with the
//! default models; every refusal comes back as a reason and a missing program as a failure.

use super::CliLauncher;
use orq_remote::projects::{LaunchError, RunLauncher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const MODELS: &str = "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[roles]\nimplementer = \"opus\"\nreviewer = \"opus\"\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-launcher-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(["init", "-q", "-b", "main"]).status().unwrap().success());
    repo
}

/// A stand-in for `orq`: logs its arguments, refuses `--check` when `refuse` exists, prints a run id otherwise.
fn fake_orq(dir: &Path) -> PathBuf {
    let path = dir.join("orq");
    let log = dir.join("calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> {log}\nif [ -e {hang} ]; then exec sleep 30; fi\nif [ -e {flood} ]; then exec yes; fi\ncase \"$*\" in\n*--check*) if [ -e {refuse} ]; then echo 'orq: feature needs a plan in \"/home/me/repo/plan.json\" (path=/home/me/repo)' >&2; exit 1; fi; exit 0;;\n*--detach*) if [ -e {detach_fails} ]; then echo 'orq: cannot start /x' >&2; exit 1; fi; if [ -e {no_id} ]; then echo started; exit 0; fi; echo 'run: run-42';;\nesac\n",
        log = log.display(),
        refuse = dir.join("refuse").display(),
        hang = dir.join("hang").display(),
        flood = dir.join("flood").display(),
        detach_fails = dir.join("detach-fails").display(),
        no_id = dir.join("no-id").display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn a_run_is_checked_then_started_detached_with_the_default_models() {
    let dir = scratch("launch");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config.clone()));

    // No models configured: nothing is launched.
    match launcher.start(&repo, "ready") {
        Err(LaunchError::Refused(reason)) => assert!(reason.contains("model"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.join("calls.log").exists());

    std::fs::write(&config, MODELS).unwrap();
    assert_eq!(launcher.start(&repo, "ready").unwrap(), "run-42");
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("--check") && !lines[0].contains("--detach"), "{calls}");
    assert!(lines[1].contains("--detach") && !lines[1].contains("--check"), "{calls}");
    for line in &lines {
        assert!(line.contains("--feature ready") && line.contains("--implementer opus") && line.contains("--reviewer opus") && line.contains("--remote origin"), "{line}");
    }

    // The run's own refusal is the reason, and nothing starts after it.
    std::fs::write(dir.join("refuse"), "").unwrap();
    match launcher.start(&repo, "ready") {
        Err(LaunchError::Refused(reason)) => assert_eq!(reason, "feature needs a plan in <path> <path>"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(dir.join("calls.log")).unwrap().lines().count(), 3);

    // A program that is not there is a failure, not a refusal.
    let missing = CliLauncher::new(dir.join("no-such-orq"), Some(config));
    assert!(matches!(missing.start(&repo, "ready"), Err(LaunchError::Failed(_))));
}

#[test]
fn a_stalled_or_endless_orq_is_stopped_at_the_deadline_and_reported_as_a_failure() {
    let dir = scratch("deadline");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, MODELS).unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config)).with_deadline(std::time::Duration::from_millis(500));
    for mode in ["hang", "flood"] {
        std::fs::write(dir.join(mode), "").unwrap();
        let began = std::time::Instant::now();
        assert!(matches!(launcher.start(&repo, "ready"), Err(LaunchError::Failed(_))), "{mode}");
        assert!(began.elapsed() < std::time::Duration::from_secs(10), "{mode} took {:?}", began.elapsed());
        std::fs::remove_file(dir.join(mode)).unwrap();
    }
}

#[test]
fn a_detached_start_that_fails_or_names_no_run_is_a_failure_and_is_not_retried() {
    let dir = scratch("detach");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, MODELS).unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config));
    for (mode, reason) in [("detach-fails", "cannot start /x"), ("no-id", "the run started but reported no run id")] {
        std::fs::write(dir.join(mode), "").unwrap();
        let before = std::fs::read_to_string(dir.join("calls.log")).map(|log| log.lines().count()).unwrap_or(0);
        match launcher.start(&repo, "ready") {
            Err(LaunchError::Failed(found)) => assert_eq!(found, reason, "{mode}"),
            other => panic!("{mode}: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(dir.join("calls.log")).unwrap().lines().count(), before + 2, "{mode}: one check and one start, no retry");
        std::fs::remove_file(dir.join(mode)).unwrap();
    }
}
