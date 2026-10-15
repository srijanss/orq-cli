use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use orq_features::{run_generation, AgentProfile, GenerationError, GenerationRequest};

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-genbounds-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(program: &std::path::Path, label: &str, timeout: Duration) -> (Result<PathBuf, GenerationError>, PathBuf) {
    let root = dir(label);
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let artifacts = root.join("out");
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };
    let result = run_generation(&GenerationRequest {
        program,
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &artifacts,
        timeout,
    });
    (result, artifacts)
}

#[test]
fn a_descendant_holding_stdout_open_cannot_outlive_the_timeout() {
    let script_dir = dir("descendant-bin");
    let script = script_dir.join("agent");
    fs::write(&script, "#!/bin/sh\nsleep 30 &\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let started = Instant::now();

    let (result, _) = run(&script, "descendant", Duration::from_millis(500));

    assert!(matches!(result, Err(GenerationError::TimedOut)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
}
