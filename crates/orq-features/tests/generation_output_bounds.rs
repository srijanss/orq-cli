use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{run_generation, AgentProfile, GenerationError, GenerationRequest};
use orq_test_support::FakeAgent;

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
    let profile = AgentProfile { harness: "claude".into(), profile: "writer".into() };
    let result = run_generation(&GenerationRequest {
        program,
        profile: &profile,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &artifacts,
        timeout,
    });
    (result, artifacts)
}

#[test]
fn output_over_the_limit_is_an_error_and_no_truncated_artifact_is_kept() {
    let big = format!("[{}]", "1,".repeat(2 * 1024 * 1024 + 8));
    let agent = FakeAgent::builder().stdout(&big).build();

    let (result, artifacts) = run(agent.path(), "oversize", Duration::from_secs(20));

    let err = result.unwrap_err();
    assert!(matches!(err, GenerationError::OutputTooLarge), "{err:?}");
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(!artifacts.exists() || fs::read_dir(&artifacts).unwrap().next().is_none());
}

fn output_of(len: usize) -> String {
    format!("[{}]", " ".repeat(len - 2))
}

#[test]
fn output_at_or_just_under_the_limit_is_kept_byte_for_byte() {
    const LIMIT: usize = 4 * 1024 * 1024;
    for (label, len) in [("at-limit", LIMIT), ("under-limit", LIMIT - 1)] {
        let text = output_of(len);
        let agent = FakeAgent::builder().stdout(&text).build();

        let (result, _) = run(agent.path(), label, Duration::from_secs(30));

        let path = result.unwrap_or_else(|e| panic!("{label}: {e}"));
        let stored = fs::read(&path).unwrap();
        assert!(stored == text.as_bytes(), "{label}: stored {} bytes, expected {}", stored.len(), len);
    }
}

#[test]
fn output_one_byte_over_the_limit_is_rejected() {
    const LIMIT: usize = 4 * 1024 * 1024;
    let agent = FakeAgent::builder().stdout(&output_of(LIMIT + 1)).build();

    let (result, _) = run(agent.path(), "one-over", Duration::from_secs(30));

    assert!(matches!(result, Err(GenerationError::OutputTooLarge)), "{result:?}");
}
