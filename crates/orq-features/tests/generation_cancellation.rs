use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orq_features::{run_generation_cancellable, AgentProfile, GenerationError, GenerationRequest};
use orq_test_support::FakeAgent;

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gencancel-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_cancelled_generation_stops_the_agent_promptly_and_reports_it() {
    let root = dir("hang");
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let agent = FakeAgent::builder().hang().build();
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };
    let artifacts = root.join("out");
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();

    let result = run_generation_cancellable(
        &GenerationRequest {
            program: agent.path(),
            model: &model,
            repo_root: &root,
            spec: &spec,
            artifact_dir: &artifacts,
            timeout: Duration::from_secs(60),
        },
        &cancel,
    );

    assert!(matches!(result, Err(GenerationError::Cancelled)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    assert!(!artifacts.exists() || fs::read_dir(&artifacts).unwrap().next().is_none());
}

#[test]
fn an_unset_flag_does_not_disturb_a_normal_run() {
    let root = dir("normal");
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let agent = FakeAgent::builder().stdout("[]").build();
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };

    let result = run_generation_cancellable(
        &GenerationRequest {
            program: agent.path(),
            model: &model,
            repo_root: &root,
            spec: &spec,
            artifact_dir: &root.join("out"),
            timeout: Duration::from_secs(10),
        },
        &AtomicBool::new(false),
    );

    assert!(result.is_ok(), "{result:?}");
}
