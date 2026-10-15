#![cfg(target_os = "linux")]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use orq_features::{run_generation, AgentProfile, GenerationError, GenerationRequest};

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gengroup-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `pid` is alive and not a zombie. Needs procfs, so these tests are
/// Linux-only; any read error other than "no such process" is a test failure
/// rather than proof the process stopped.
fn running(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat.rsplit(')').next().map(|rest| !rest.trim_start().starts_with('Z')).unwrap_or(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("cannot inspect process {pid}: {e}"),
    }
}

fn generate(label: &str, body: &str, timeout: Duration) -> (Result<PathBuf, GenerationError>, Descendant) {
    let root = dir(label);
    let pid_file = root.join("descendant.pid");
    let script = root.join("agent");
    let text = body.replace("PIDFILE", &format!("'{}'", pid_file.display()));
    fs::write(&script, format!("#!/bin/sh\n{text}\n")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };

    let result = run_generation(&GenerationRequest {
        program: &script,
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &root.join("out"),
        timeout,
        focus: None,
    });
    let pid = fs::read_to_string(&pid_file).expect("the agent recorded its descendant").trim().to_string();
    (result, Descendant(pid))
}

/// Kills the descendant on drop, so a failing assertion cannot leave it running.
struct Descendant(String);

impl Drop for Descendant {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill").arg(&self.0).status();
    }
}

fn assert_descendant_stops(descendant: &Descendant) {
    let pid = descendant.0.as_str();
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = running(pid);
    assert!(!survived, "descendant {pid} outlived the generation");
}

#[test]
fn a_timed_out_generation_terminates_the_agents_descendants() {
    let (result, pid) =
        generate("timeout", "sleep 60 &\necho $! > PIDFILE\nwait", Duration::from_secs(5));

    assert!(matches!(result, Err(GenerationError::TimedOut)), "{result:?}");
    assert_descendant_stops(&pid);
}

#[test]
fn a_descendant_holding_stdout_after_the_agent_exits_is_terminated() {
    let (result, pid) = generate("exited", "sleep 60 &\necho $! > PIDFILE\nexit 0", Duration::from_secs(5));

    assert!(matches!(result, Err(GenerationError::TimedOut)), "{result:?}");
    assert_descendant_stops(&pid);
}

#[test]
fn an_output_overflow_terminates_a_live_descendant() {
    let body = "sleep 60 >/dev/null 2>&1 &\necho $! > PIDFILE\nhead -c 5000000 /dev/zero\nsleep 30";

    let (result, pid) = generate("overflow", body, Duration::from_secs(20));

    assert!(matches!(result, Err(GenerationError::OutputTooLarge)), "{result:?}");
    assert_descendant_stops(&pid);
}

#[test]
fn a_successful_generation_terminates_a_descendant_that_redirected_stdout() {
    let body = "sleep 60 >/dev/null 2>&1 &\necho $! > PIDFILE\nprintf '[]'";

    let (result, pid) = generate("success", body, Duration::from_secs(20));

    assert!(result.is_ok(), "{result:?}");
    assert_descendant_stops(&pid);
}

#[test]
fn a_failing_agent_exit_terminates_a_redirected_descendant() {
    let body = "sleep 60 >/dev/null 2>&1 &\necho $! > PIDFILE\nexit 3";

    let (result, pid) = generate("nonzero", body, Duration::from_secs(20));

    assert!(matches!(result, Err(GenerationError::AgentFailed { code: Some(3) })), "{result:?}");
    assert_descendant_stops(&pid);
}
