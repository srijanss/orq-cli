use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{
    generation_artifact_dir, run_generation, AgentProfile, GenerationError, GenerationRequest,
};
use orq_test_support::FakeAgent;

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-gen-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn model() -> AgentProfile {
    AgentProfile { harness: "claude".into(), model: "writer".into() }
}

fn request<'a>(
    program: &'a std::path::Path,
    model: &'a AgentProfile,
    root: &'a std::path::Path,
    spec: &'a std::path::Path,
    artifacts: &'a std::path::Path,
) -> GenerationRequest<'a> {
    GenerationRequest {
        program,
        model,
        repo_root: root,
        spec,
        artifact_dir: artifacts,
        timeout: Duration::from_secs(10),
        focus: None,
    }
}

#[test]
fn the_agent_gets_an_argument_vector_with_spec_profile_and_schema() {
    let root = dir("argv");
    let spec = root.join("my spec; rm -rf x.md");
    fs::write(&spec, "# spec").unwrap();
    let agent = FakeAgent::builder().stdout("[]").build();
    let model = model();
    let artifacts = dir("argv-art");

    run_generation(&request(agent.path(), &model, &root, &spec, &artifacts)).unwrap();

    let run = agent.last_run().unwrap();
    assert_eq!(run.cwd.canonicalize().unwrap(), root.canonicalize().unwrap());
    // The model is a discrete argument, and the spec path (with shell
    // metacharacters) appears intact inside exactly one argument.
    assert!(run.args.windows(2).any(|w| w == ["--model", "writer"]), "{:?}", run.args);
    let prompt = run.args.iter().find(|a| a.contains("featureName")).expect("schema in prompt");
    assert!(prompt.contains(spec.to_str().unwrap()));
    assert!(prompt.contains("dependsOn"));
    assert!(prompt.contains("parallelizable"));
    assert!(prompt.contains("Do not modify"), "{prompt}");
}

#[test]
fn stdout_is_stored_in_an_owner_only_artifact() {
    let root = dir("art");
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let agent = FakeAgent::builder().stdout(r#"[{"featureName":"a","status":"draft"}]"#).build();
    let model = model();
    let artifacts = dir("art-out");

    let artifact = run_generation(&request(agent.path(), &model, &root, &spec, &artifacts)).unwrap();

    assert!(artifact.starts_with(&artifacts));
    assert_eq!(fs::read_to_string(&artifact).unwrap(), r#"[{"featureName":"a","status":"draft"}]"#);
    let mode = fs::metadata(&artifact).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_missing_agent_is_an_actionable_error() {
    let root = dir("missing");
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let model = model();
    let artifacts = dir("missing-art");
    let program = root.join("no-such-agent");

    let err = run_generation(&request(&program, &model, &root, &spec, &artifacts)).unwrap_err();

    assert!(matches!(err, GenerationError::AgentNotFound { .. }), "{err:?}");
    let message = err.to_string();
    assert!(message.contains("claude") && message.contains("Settings"), "{message}");
}

#[test]
fn a_failing_or_hanging_agent_is_reported_and_leaves_no_artifact() {
    let root = dir("fail");
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let model = model();
    let artifacts = dir("fail-art");

    let failing = FakeAgent::builder().stdout("partial").exit_code(3).build();
    let err = run_generation(&request(failing.path(), &model, &root, &spec, &artifacts)).unwrap_err();
    assert!(matches!(err, GenerationError::AgentFailed { code: Some(3) }), "{err:?}");

    let hanging = FakeAgent::builder().hang().build();
    let mut req = request(hanging.path(), &model, &root, &spec, &artifacts);
    req.timeout = Duration::from_millis(300);
    let err = run_generation(&req).unwrap_err();
    assert!(matches!(err, GenerationError::TimedOut), "{err:?}");

    assert_eq!(fs::read_dir(&artifacts).unwrap().count(), 0);
}

#[test]
fn artifact_dir_follows_xdg_cache_then_home() {
    assert_eq!(
        generation_artifact_dir(Some("/x/cache".into()), Some("/home/u".into())),
        Some(PathBuf::from("/x/cache/orq/generation"))
    );
    assert_eq!(
        generation_artifact_dir(None, Some("/home/u".into())),
        Some(PathBuf::from("/home/u/.cache/orq/generation"))
    );
    assert_eq!(generation_artifact_dir(None, None), None);
}
