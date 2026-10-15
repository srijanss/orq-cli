#![cfg(target_os = "linux")]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orq_features::{run_generation_cancellable, AgentProfile, GenerationError, GenerationRequest};

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gencancel-desc-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `pid` is alive and not a zombie.
fn running(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat.rsplit(')').next().map(|rest| !rest.trim_start().starts_with('Z')).unwrap_or(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("cannot inspect process {pid}: {e}"),
    }
}

/// Kills the descendant on drop, so a failing assertion cannot leave it running.
struct Descendant(String);

impl Drop for Descendant {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill").arg(&self.0).status();
    }
}

/// Runs `body` as the agent, cancels once it has recorded its descendant's pid.
fn cancel_run(label: &str, body: &str) -> (Result<PathBuf, GenerationError>, Descendant, PathBuf, Duration) {
    let root = dir(label);
    let pid_file = root.join("descendant.pid");
    let script = root.join("agent");
    let text = body.replace("PIDFILE", &format!("'{}'", pid_file.display()));
    fs::write(&script, format!("#!/bin/sh\n{text}\n")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let profile = AgentProfile { harness: "claude".into(), profile: "writer".into() };
    let artifacts = root.join("out");

    let cancel = Arc::new(AtomicBool::new(false));
    let (flag, watched) = (Arc::clone(&cancel), pid_file.clone());
    std::thread::spawn(move || {
        // Cancel only once the descendant exists, so the test cannot race the agent.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watched.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let result = run_generation_cancellable(
        &GenerationRequest {
            program: &script,
            profile: &profile,
            repo_root: &root,
            spec: &spec,
            artifact_dir: &artifacts,
            timeout: Duration::from_secs(60),
        },
        &cancel,
    );
    let elapsed = started.elapsed();
    let pid = fs::read_to_string(&pid_file).expect("the agent recorded its descendant").trim().to_string();
    (result, Descendant(pid), artifacts, elapsed)
}

fn assert_cancelled_cleanly(result: Result<PathBuf, GenerationError>, pid: &Descendant, artifacts: &PathBuf, took: Duration) {
    assert!(matches!(result, Err(GenerationError::Cancelled)), "{result:?}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while running(&pid.0) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!running(&pid.0), "descendant {} survived the cancel", pid.0);
    assert!(!artifacts.exists() || fs::read_dir(artifacts).unwrap().next().is_none());
}

#[test]
fn cancelling_a_hanging_agent_terminates_its_descendants() {
    let (result, pid, artifacts, took) = cancel_run("hang", "sleep 60 &\necho $! > PIDFILE\nwait");
    assert_cancelled_cleanly(result, &pid, &artifacts, took);
}

#[test]
fn cancelling_after_the_agent_exits_terminates_a_descendant_holding_stdout() {
    let (result, pid, artifacts, took) = cancel_run("exited", "sleep 60 &\necho $! > PIDFILE\nexit 0");
    assert_cancelled_cleanly(result, &pid, &artifacts, took);
}
