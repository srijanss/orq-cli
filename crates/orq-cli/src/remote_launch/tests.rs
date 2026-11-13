//! The web server's launcher asks `orq run --check` first, then starts the run detached, with the
//! models the run picks itself unless profiles were chosen; every refusal comes back as a reason and a missing program as a failure.

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
        "#!/bin/sh\necho \"$*\" >> {log}\nif [ -e {hang} ]; then exec sleep 30; fi\nif [ -e {flood} ]; then exec yes; fi\nif [ -e {orphan} ]; then sleep 20 & echo $! > {pid}; exit 0; fi\ncase \"$*\" in\n*--check*) if [ -e {refuse} ]; then echo 'orq: feature needs a plan in \"/home/me/repo/plan.json\" (path=/home/me/repo)' >&2; exit 1; fi; exit 0;;\n*--detach*) if [ -e {big} ]; then echo 'run: run-42'; head -c 300000 /dev/zero | tr '\\0' x; head -c 300000 /dev/zero | tr '\\0' x >&2 && touch {done}; exit 0; fi; if [ -e {detach_fails} ]; then echo 'orq: cannot start /x' >&2; exit 1; fi; if [ -e {no_id} ]; then echo started; exit 0; fi; echo 'run: run-42';;\nesac\n",
        log = log.display(),
        refuse = dir.join("refuse").display(),
        hang = dir.join("hang").display(),
        flood = dir.join("flood").display(),
        big = dir.join("big").display(),
        done = dir.join("drained").display(),
        orphan = dir.join("orphan").display(),
        pid = dir.join("orphan.pid").display(),
        detach_fails = dir.join("detach-fails").display(),
        no_id = dir.join("no-id").display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn a_run_is_checked_then_started_detached_leaving_the_default_models_to_the_run() {
    let dir = scratch("launch");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config.clone()));

    // Roles are configured, yet the run is the one that picks them, as it does on the command line.
    std::fs::write(&config, MODELS).unwrap();
    assert_eq!(launcher.start(&repo, "ready").unwrap(), "run-42");
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("--check") && !lines[0].contains("--detach"), "{calls}");
    assert!(lines[1].contains("--detach") && !lines[1].contains("--check"), "{calls}");
    // While Herdr is running the run goes in a pane of its own, where its session can be answered.
    assert!(lines[1].contains("--in-herdr") && !lines[0].contains("--in-herdr"), "{calls}");
    for line in &lines {
        assert!(line.contains("--feature ready") && !line.contains("--implementer") && !line.contains("--reviewer") && line.contains("--remote origin"), "{line}");
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

#[test]
fn the_deadline_is_one_budget_for_the_whole_call_even_when_a_descendant_holds_the_pipes() {
    let dir = scratch("budget");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, MODELS).unwrap();
    std::fs::write(dir.join("orphan"), "").unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config)).with_deadline(std::time::Duration::from_secs(1));
    let began = std::time::Instant::now();
    assert!(matches!(launcher.start(&repo, "ready"), Err(LaunchError::Failed(_))));
    assert!(began.elapsed() < std::time::Duration::from_millis(1600), "took {:?}", began.elapsed());
}

#[test]
fn a_descendant_left_holding_the_pipes_is_stopped_with_the_call() {
    let dir = scratch("orphan");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, MODELS).unwrap();
    std::fs::write(dir.join("orphan"), "").unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config)).with_deadline(std::time::Duration::from_millis(500));
    assert!(matches!(launcher.start(&repo, "ready"), Err(LaunchError::Failed(_))));
    let pid = std::fs::read_to_string(dir.join("orphan.pid")).unwrap().trim().to_string();
    let alive = || std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|stat| !stat.contains(") Z")).unwrap_or(false);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while alive() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!alive(), "the descendant {pid} outlived the call");
}

#[test]
fn finite_output_past_the_cap_on_both_streams_is_drained_and_the_run_id_survives() {
    let dir = scratch("big");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, MODELS).unwrap();
    std::fs::write(dir.join("big"), "").unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config)).with_deadline(std::time::Duration::from_secs(3));
    assert_eq!(launcher.start(&repo, "ready").unwrap(), "run-42");
    assert!(dir.join("drained").exists(), "the caller stopped reading before the program finished writing");
}

#[test]
fn a_resume_runs_orq_run_resume_detached_in_herdr_and_its_refusal_is_the_reason() {
    let dir = scratch("resume");
    let repo = repo(&dir);
    let exe = dir.join("orq");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> {log}\nif [ -e {refuse} ]; then echo 'orq: cannot resume r-1: the checkpoint no longer matches /home/me/wt' >&2; exit 1; fi\nif [ -e {broken} ]; then echo 'orq: cannot read the run: Io(\"/home/me/state\")' >&2; exit 1; fi\nif [ -e {no_id} ]; then echo resumed; exit 0; fi\necho 'run: run-77'\n",
        log = dir.join("calls.log").display(),
        broken = dir.join("broken").display(),
        refuse = dir.join("refuse").display(),
        no_id = dir.join("no-id").display()
    );
    std::fs::write(&exe, script).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let launcher = CliLauncher::new(exe, None);

    assert_eq!(launcher.resume(&repo, "r-1").unwrap(), "run-77");
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    // While Herdr is running the resumed run goes in a pane of its own, as a started one does.
    assert_eq!(calls.trim_end(), format!("run resume r-1 --yes --detach --in-herdr --repo {}", repo.display()));

    // The command's own refusal is the reason, without the machine's paths.
    std::fs::write(dir.join("refuse"), "").unwrap();
    match launcher.resume(&repo, "r-1") {
        Err(LaunchError::Refused(reason)) => assert_eq!(reason, "cannot resume r-1: the checkpoint no longer matches <path>"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(dir.join("refuse")).unwrap();

    // Any other failure of the command is the operator's, never a reason for the web.
    std::fs::write(dir.join("broken"), "").unwrap();
    assert!(matches!(launcher.resume(&repo, "r-1"), Err(LaunchError::Failed(_))));
    std::fs::remove_file(dir.join("broken")).unwrap();

    // A resume that names no run, or a program that is not there, is a failure.
    std::fs::write(dir.join("no-id"), "").unwrap();
    assert!(matches!(launcher.resume(&repo, "r-1"), Err(LaunchError::Failed(_))));
    let missing = CliLauncher::new(dir.join("no-such-orq"), None);
    assert!(matches!(missing.resume(&repo, "r-1"), Err(LaunchError::Failed(_))));
    // A run id that could be read as an option never reaches the command.
    assert!(matches!(launcher.resume(&repo, "--yes"), Err(LaunchError::Refused(_))));
    assert_eq!(std::fs::read_to_string(dir.join("calls.log")).unwrap().lines().count(), 4);
}

#[test]
fn a_start_with_chosen_profiles_uses_them_instead_of_the_defaults_and_checks_them_like_the_cli() {
    let dir = scratch("launch-with");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    // No default roles at all: a choice does not need them.
    std::fs::write(&config, "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n").unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config));
    let chosen = |implementer: &str, reviewer: &str| orq_core::remote_inbox::LaunchProfiles { implementer: implementer.into(), reviewer: reviewer.into() };

    assert_eq!(launcher.start_with(&repo, "ready", &chosen("sonnet", "opus")).unwrap(), "run-42");
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("--check") && lines[1].contains("--detach"), "{calls}");
    for line in &lines {
        assert!(line.contains("--feature ready") && line.contains("--implementer sonnet") && line.contains("--reviewer opus"), "{line}");
    }

    // A profile the models do not define is refused before anything runs.
    match launcher.start_with(&repo, "ready", &chosen("gpt", "opus")) {
        Err(LaunchError::Refused(reason)) => assert!(reason.contains("gpt"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(dir.join("calls.log")).unwrap().lines().count(), 2);
}

/// A start with no profiles chosen names none, so `orq run` picks the agents itself as it does on the
/// command line: the project's own, else the machine's. No matching profile is needed to start.
#[test]
fn a_start_without_a_choice_names_no_profiles_so_the_run_picks_the_projects_agents() {
    let dir = scratch("launch-project-default");
    let repo = repo(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, "[implementer]\nharness = \"pi\"\nmodel = \"glm\"\n").unwrap();
    let launcher = CliLauncher::new(fake_orq(&dir), Some(config));

    assert_eq!(launcher.start(&repo, "ready").unwrap(), "run-42");
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(lines[0].contains("--check") && lines[1].contains("--detach"), "{calls}");
    for line in &lines {
        assert!(line.contains("--feature ready") && !line.contains("--implementer") && !line.contains("--reviewer"), "{line}");
    }
}

/// The launcher warns, as `orq run --check` does, when the local target branch has commits the remote's
/// does not, since the run starts from the local one and its review carries them.
#[test]
fn a_start_is_warned_about_local_target_commits_its_remote_lacks() {
    let dir = scratch("ahead");
    let repo = repo(&dir);
    let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).status().unwrap().success());
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let launcher = CliLauncher::new(fake_orq(&dir), None);
    assert_eq!(launcher.start_warning(&repo), None, "even with origin/main");

    git(&["commit", "-q", "--allow-empty", "-m", "local"]);
    assert_eq!(launcher.start_warning(&repo).as_deref(), Some("main has 1 commit origin/main does not; the run starts from main with them, and its review carries them too"));
}
