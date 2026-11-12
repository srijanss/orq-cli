//! The process that owns a run takes what it started down with it, whichever way it was running that
//! child and whichever ending signal arrives. Each `#[ignore]`d test is not a test of its own: it is
//! the owner a real test starts as a separate process, because the owner has to die of the signal.

use std::collections::BTreeMap;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orq_orchestrator::{
    draft_review_description, drop_test_databases_within, end_children_with_this_process,
    run_in_terminal, run_setup, DraftSession, RunRecord, RunStore,
};

const PID_FILE_VARIABLE: &str = "ORQ_TEST_CHILD_PID_FILE";
const LONG: Duration = Duration::from_secs(30);

fn scratch(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-stopped-owner-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A script that notes the signals it was started ignoring (a background job would change its own),
/// starts a long-lived child, writes the child's process id down, and waits for it.
fn starts_a_child(pidfile: &Path) -> String {
    let pidfile = pidfile.display();
    format!("grep SigIgn /proc/$$/status > '{pidfile}.ignored'; sleep 300 & echo $! > '{pidfile}'; wait")
}

/// The signals the owner's child noted it was started ignoring, as the kernel's bit mask.
fn ignored_by_child(pidfile: &Path) -> u64 {
    let text = std::fs::read_to_string(format!("{}.ignored", pidfile.display())).unwrap();
    let mask = text.trim().strip_prefix("SigIgn:").expect("a SigIgn line").trim();
    u64::from_str_radix(mask, 16).unwrap()
}

/// In an owner process: its scratch directory and child script, with the ending signals set up.
/// `None` when this is the ordinary test run reaching an ignored test, which then does nothing.
fn as_the_owner() -> Option<(PathBuf, String)> {
    let pidfile = PathBuf::from(std::env::var_os(PID_FILE_VARIABLE)?);
    end_children_with_this_process();
    let script = starts_a_child(&pidfile);
    Some((pidfile.parent().unwrap().to_path_buf(), script))
}

#[test]
#[ignore = "started as a separate process by the tests below"]
fn owner_running_a_setup_step() {
    let Some((dir, script)) = as_the_owner() else { return };
    let step = vec!["sh".to_string(), "-c".to_string(), script];
    let _ = run_setup(&dir, &[step], &dir.join("setup.log"));
}

#[test]
#[ignore = "started as a separate process by the tests below"]
fn owner_running_a_terminal_session() {
    let Some((dir, script)) = as_the_owner() else { return };
    let mut command = Command::new("sh");
    command.args(["-c", &script]).current_dir(&dir);
    let _ = run_in_terminal(command, &dir.join("never"), LONG, LONG);
}

#[test]
#[ignore = "started as a separate process by the tests below"]
fn owner_running_a_database_drop() {
    let Some((dir, script)) = as_the_owner() else { return };
    let command = ["sh".to_string(), "-c".to_string(), script];
    let databases = [("DB_NAME".to_string(), "shop_fix_a".to_string())];
    let _ = drop_test_databases_within(&dir, &databases, Some(&command), false, LONG);
}

#[test]
#[ignore = "started as a separate process by the tests below"]
fn owner_running_a_review_draft() {
    let Some((dir, script)) = as_the_owner() else { return };
    let worktree = dir.join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q", "-b", "main"]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "feat: the change"]);
    let store = RunStore::new(&dir.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base,
            branch: "main".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let _ = draft_review_description(&store, "run-1", LONG, |_prompt| DraftSession {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script],
        env: Vec::new(),
        cwd: worktree,
    });
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Executing: there is such a process and it is not a zombie nothing has reaped yet.
fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(stat) => {
            let state = stat
                .rfind(')')
                .and_then(|at| stat[at + 1..].split_whitespace().next().map(str::to_string));
            !matches!(state.as_deref(), Some("Z" | "X" | "x"))
        }
    }
}

fn gone_soon(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

fn send(pid: u32, signal: i32) {
    // Safety: plain system call on a process this test started.
    unsafe { libc::kill(pid as i32, signal) };
}

fn command_for_owner(owner: &str, pidfile: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", owner, "--ignored", "--nocapture"])
        .env(PID_FILE_VARIABLE, pidfile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The process the owner's child wrote down, once it has.
fn started_child(owner: &mut Child, pidfile: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(pid) = std::fs::read_to_string(pidfile).ok().and_then(|text| text.trim().parse().ok()) {
            return pid;
        }
        if let Some(status) = owner.try_wait().unwrap() {
            panic!("the owner ended ({status}) before it started the child");
        }
        assert!(Instant::now() < deadline, "the owner never started the child");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sends `signal` to the owner and checks the child went, and that the owner died of that signal.
fn ends_with_the_owner(owner: &mut Child, child: i32, signal: i32) {
    send(owner.id(), signal);
    let gone = gone_soon(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match owner.try_wait().unwrap() {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Never leave anything behind for the rest of the test run.
    if !gone {
        send(child as u32, libc::SIGKILL);
    }
    let _ = owner.kill();
    let _ = owner.wait();
    assert!(gone, "process {child} outlived its owner");
    let status = status.expect("the owner did not end");
    assert_eq!(status.signal(), Some(signal), "the owner should die of the signal: {status}");
}

fn stopping_the_owner_ends_its_child(owner: &str, signal: i32) {
    let dir = scratch(&format!("{owner}-{signal}"));
    let pidfile = dir.join("child.pid");
    let mut process = command_for_owner(owner, &pidfile).spawn().unwrap();
    let child = started_child(&mut process, &pidfile);
    ends_with_the_owner(&mut process, child, signal);
}

#[test]
fn a_stop_ends_what_a_setup_step_started() {
    stopping_the_owner_ends_its_child("owner_running_a_setup_step", libc::SIGTERM);
}

#[test]
fn a_stop_ends_what_a_terminal_session_started() {
    stopping_the_owner_ends_its_child("owner_running_a_terminal_session", libc::SIGTERM);
}

#[test]
fn a_stop_ends_what_a_database_drop_started() {
    stopping_the_owner_ends_its_child("owner_running_a_database_drop", libc::SIGTERM);
}

#[test]
fn a_stop_ends_what_a_review_draft_started() {
    stopping_the_owner_ends_its_child("owner_running_a_review_draft", libc::SIGTERM);
}

#[test]
fn an_interrupt_ends_what_the_owner_started() {
    stopping_the_owner_ends_its_child("owner_running_a_setup_step", libc::SIGINT);
}

#[test]
fn a_hangup_ends_what_the_owner_started() {
    stopping_the_owner_ends_its_child("owner_running_a_setup_step", libc::SIGHUP);
}

/// An owner started with a signal ignored (`nohup orq run`) keeps ignoring it, and so does its child.
#[test]
fn a_signal_the_owner_was_started_ignoring_stays_ignored() {
    let dir = scratch("ignored");
    let pidfile = dir.join("child.pid");
    let mut command = command_for_owner("owner_running_a_setup_step", &pidfile);
    // Safety: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut owner = command.spawn().unwrap();
    let child = started_child(&mut owner, &pidfile);

    send(owner.id(), libc::SIGINT);
    send(owner.id(), libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(500));

    let owner_still_running = owner.try_wait().unwrap().is_none();
    let ignored = ignored_by_child(&pidfile);
    // SIGTERM was not ignored, so this also cleans up.
    ends_with_the_owner(&mut owner, child, libc::SIGTERM);
    assert!(owner_still_running, "an ignored signal ended the owner");
    for signal in [libc::SIGINT, libc::SIGHUP] {
        assert_ne!(ignored & (1 << (signal - 1)), 0, "the child stopped ignoring signal {signal}");
    }
}

/// A setup step may start a service the run goes on to use, so finishing normally must not end it.
#[test]
fn a_setup_step_that_finishes_leaves_what_it_started_running() {
    let dir = scratch("service");
    let pidfile = dir.join("service.pid");
    let script = format!("sleep 300 >/dev/null 2>&1 & echo $! > '{}'", pidfile.display());
    let step = vec!["sh".to_string(), "-c".to_string(), script];

    let result = run_setup(&dir, &[step], &dir.join("setup.log"));

    let service: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let running = alive(service);
    send(service as u32, libc::SIGKILL);
    assert!(result.is_ok(), "the setup step failed");
    assert!(running, "the setup step's service did not outlive the step");
}
