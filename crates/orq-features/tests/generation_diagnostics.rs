use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use orq_features::{run_generation_diagnosed, AgentProfile, GenerationError, GenerationRequest};
use orq_test_support::FakeAgent;

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gendiag-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn model() -> AgentProfile {
    AgentProfile { harness: "claude".into(), model: "writer".into() }
}

fn run(
    agent: &FakeAgent,
    label: &str,
    timeout: Duration,
) -> (Result<PathBuf, GenerationError>, orq_features::RunDiagnostics) {
    let root = dir(label);
    let spec = root.join("SPEC.md");
    fs::write(&spec, "# spec").unwrap();
    let artifacts = dir(&format!("{label}-art"));
    let model = model();
    let request = GenerationRequest {
        program: agent.path(),
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &artifacts,
        timeout,
        focus: None,
    };
    run_generation_diagnosed(&request, &AtomicBool::new(false))
}

#[test]
fn a_failed_agent_leaves_its_stderr_and_exit_status() {
    let agent = FakeAgent::builder().stderr("Not logged in\n").exit_code(3).build();

    let (result, diagnostics) = run(&agent, "fail", Duration::from_secs(10));

    assert!(matches!(result, Err(GenerationError::AgentFailed { code: Some(3) })));
    assert!(diagnostics.stderr_tail.contains("Not logged in"), "{diagnostics:?}");
    assert!(diagnostics.exit.contains('3'), "{diagnostics:?}");
}

#[test]
fn stdout_head_is_kept_even_when_the_run_fails() {
    let agent = FakeAgent::builder().stdout("partial output").exit_code(1).build();

    let (_, diagnostics) = run(&agent, "head", Duration::from_secs(10));

    assert!(diagnostics.stdout_head.contains("partial output"), "{diagnostics:?}");
}

#[test]
fn a_successful_run_still_returns_the_artifact() {
    let agent = FakeAgent::builder().stdout("[]").stderr("warning: slow\n").build();

    let (result, diagnostics) = run(&agent, "ok", Duration::from_secs(10));

    assert!(result.is_ok());
    assert!(diagnostics.stderr_tail.contains("warning: slow"));
}

#[test]
fn the_stderr_tail_is_bounded_and_keeps_the_end() {
    let noisy = format!("{}END-MARKER\n", "x".repeat(200_000));
    let agent = FakeAgent::builder().stderr(&noisy).exit_code(1).build();

    let (_, diagnostics) = run(&agent, "tail", Duration::from_secs(10));

    assert!(diagnostics.stderr_tail.len() <= 20 * 1024, "{}", diagnostics.stderr_tail.len());
    assert!(diagnostics.stderr_tail.contains("END-MARKER"));
}

#[test]
fn a_hanging_noisy_agent_still_times_out_promptly() {
    let agent = FakeAgent::builder().stderr("busy\n").hang().build();
    let started = Instant::now();

    let (result, diagnostics) = run(&agent, "hang", Duration::from_millis(400));

    assert!(matches!(result, Err(GenerationError::TimedOut)));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(diagnostics.stderr_tail.contains("busy"), "{diagnostics:?}");
}
