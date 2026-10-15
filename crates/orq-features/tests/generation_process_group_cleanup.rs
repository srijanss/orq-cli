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

/// A process that exists and is not a zombie waiting to be reaped.
fn running(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat.rsplit(')').next().map(|rest| !rest.trim_start().starts_with('Z')).unwrap_or(false),
        Err(_) => false,
    }
}

#[test]
fn a_timed_out_generation_terminates_the_agents_descendants() {
    let root = dir("timeout");
    let pid_file = root.join("descendant.pid");
    let script = root.join("agent");
    fs::write(
        &script,
        format!("#!/bin/sh\nsleep 60 &\necho $! > '{}'\nwait\n", pid_file.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let profile = AgentProfile { harness: "claude".into(), profile: "writer".into() };

    let result = run_generation(&GenerationRequest {
        program: &script,
        profile: &profile,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &root.join("out"),
        timeout: Duration::from_millis(800),
    });

    assert!(matches!(result, Err(GenerationError::TimedOut)), "{result:?}");
    let pid = fs::read_to_string(&pid_file).expect("the agent recorded its descendant").trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = running(&pid);
    if survived {
        let _ = std::process::Command::new("kill").arg(&pid).status();
    }
    assert!(!survived, "descendant {pid} outlived the generation");
}
