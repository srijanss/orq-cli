use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{run_generation, AgentProfile, GenerationError, GenerationRequest};
use orq_test_support::FakeAgent;

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-ro-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(harness: &str, label: &str) -> (Result<PathBuf, GenerationError>, FakeAgent) {
    let root = dir(label);
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let agent = FakeAgent::builder().stdout("[]").build();
    let model = AgentProfile { harness: harness.into(), model: "writer".into() };
    let artifacts = root.join("out");
    let result = run_generation(&GenerationRequest {
        program: agent.path(),
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &artifacts,
        timeout: Duration::from_secs(10),
        focus: None,
    });
    (result, agent)
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

#[test]
fn claude_runs_with_only_read_tools_available_and_in_plan_mode() {
    let (result, agent) = run("claude", "claude");
    result.unwrap();
    let args = agent.last_run().unwrap().args;

    // `--tools` limits what exists; `--allowedTools` only skips prompts.
    assert!(has_pair(&args, "--tools", "Read,Grep,Glob"), "{args:?}");
    assert!(has_pair(&args, "--permission-mode", "plan"), "{args:?}");
    assert!(args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
    // `--bare` never reads OAuth/keychain logins, so the agent could not sign in.
    assert!(!args.iter().any(|a| a == "--bare"), "{args:?}");
    assert!(has_pair(&args, "--disallowedTools", "Bash Edit Write WebFetch NotebookEdit Agent"), "{args:?}");
    assert!(!args.iter().any(|a| a == "--allowedTools"), "{args:?}");
    assert!(!args.iter().any(|a| a.contains("dangerously") || a == "bypassPermissions"), "{args:?}");
}

#[test]
fn pi_runs_with_read_only_tools_and_no_extensions() {
    let (result, agent) = run("pi", "pi");
    result.unwrap();
    let args = agent.last_run().unwrap().args;

    assert!(has_pair(&args, "--tools", "read,grep,find,ls"), "{args:?}");
    for flag in ["--no-mcp", "--no-extensions", "--no-context-files", "--no-session"] {
        assert!(args.iter().any(|a| a == flag), "missing {flag}: {args:?}");
    }
}

#[test]
fn an_unknown_harness_is_refused_without_starting_it() {
    let (result, agent) = run("mystery", "unknown");

    let err = result.unwrap_err();
    assert!(matches!(err, GenerationError::UnsupportedHarness { .. }), "{err:?}");
    assert!(err.to_string().contains("mystery"), "{err}");
    assert!(agent.last_run().is_none());
}
