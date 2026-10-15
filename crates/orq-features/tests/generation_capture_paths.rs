use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use orq_features::{run_generation_diagnosed, AgentProfile, GenerationError, GenerationRequest, RunDiagnostics};
use orq_test_support::FakeAgent;

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gencap-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(
    agent: &FakeAgent,
    label: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> (Result<PathBuf, GenerationError>, RunDiagnostics) {
    let root = dir(label);
    let spec = root.join("SPEC.md");
    fs::write(&spec, "# spec").unwrap();
    let artifacts = dir(&format!("{label}-art"));
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };
    let request = GenerationRequest {
        program: agent.path(),
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &artifacts,
        timeout,
        focus: None,
    };
    run_generation_diagnosed(&request, cancel)
}

#[test]
fn a_timed_out_agent_still_reports_what_it_printed() {
    let agent = FakeAgent::builder().stdout("partial stdout").stderr("partial stderr\n").hang().build();

    let (result, diagnostics) = run(&agent, "timeout", Duration::from_millis(800), &AtomicBool::new(false));

    assert!(matches!(result, Err(GenerationError::TimedOut)));
    assert!(diagnostics.stdout_head.contains("partial stdout"), "{diagnostics:?}");
    assert!(diagnostics.stderr_tail.contains("partial stderr"), "{diagnostics:?}");
}

#[test]
fn a_cancelled_agent_still_reports_what_it_printed() {
    let agent = FakeAgent::builder().stdout("early stdout").stderr("early stderr\n").hang().build();
    let cancel = AtomicBool::new(false);

    let (result, diagnostics) = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(600));
            cancel.store(true, Ordering::SeqCst);
        });
        run(&agent, "cancel", Duration::from_secs(30), &cancel)
    });

    assert!(matches!(result, Err(GenerationError::Cancelled)));
    assert!(diagnostics.stdout_head.contains("early stdout"), "{diagnostics:?}");
    assert!(diagnostics.stderr_tail.contains("early stderr"), "{diagnostics:?}");
}

#[test]
fn a_large_final_burst_of_stderr_keeps_its_last_bytes() {
    let big = format!("{}\nTHE-LAST-LINE\n", "x".repeat(40_000));
    let agent = FakeAgent::builder().stderr(&big).exit_code(1).build();

    let started = Instant::now();
    let (_, diagnostics) = run(&agent, "burst", Duration::from_secs(10), &AtomicBool::new(false));

    assert!(diagnostics.stderr_tail.contains("THE-LAST-LINE"), "{}", diagnostics.stderr_tail.len());
    assert!(diagnostics.stderr_tail.len() <= 16 * 1024);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_stdout_head_is_bounded() {
    let big = "y".repeat(100_000);
    let agent = FakeAgent::builder().stdout(&big).exit_code(1).build();

    let (_, diagnostics) = run(&agent, "head", Duration::from_secs(10), &AtomicBool::new(false));

    assert_eq!(diagnostics.stdout_head.len(), 16 * 1024);
}

#[test]
fn an_overflowing_agent_is_stopped_and_keeps_its_head_and_stderr() {
    let big = "z".repeat(5 * 1024 * 1024);
    let agent = FakeAgent::builder().stdout(&big).stderr("overflow note\n").build();

    let started = Instant::now();
    let (result, diagnostics) = run(&agent, "overflow", Duration::from_secs(30), &AtomicBool::new(false));

    assert!(matches!(result, Err(GenerationError::OutputTooLarge)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(diagnostics.stdout_head.len(), 16 * 1024);
    assert!(diagnostics.stderr_tail.contains("overflow note"), "{diagnostics:?}");
}
