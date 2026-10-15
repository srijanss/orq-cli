//! The Settings "model" selects the harness's model with `--model`; neither
//! claude nor pi documents a `--profile` flag.
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{run_generation, AgentProfile, GenerationRequest};
use orq_test_support::FakeAgent;

fn args_for(harness: &str, model: &str) -> Vec<String> {
    let root: PathBuf = std::env::temp_dir().join(format!("orq-model-{}-{harness}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    let agent = FakeAgent::builder().stdout("[]").build();
    let model = AgentProfile { harness: harness.into(), model: model.into() };
    run_generation(&GenerationRequest {
        program: agent.path(),
        model: &model,
        repo_root: &root,
        spec: &spec,
        artifact_dir: &root.join("out"),
        timeout: Duration::from_secs(10),
    })
    .unwrap();
    agent.last_run().unwrap().args
}

#[test]
fn the_model_is_passed_as_the_model_flag_and_never_as_a_profile_flag() {
    for (harness, model) in [("claude", "opus"), ("pi", "openai/gpt-6-astra")] {
        let args = args_for(harness, model);
        assert!(args.windows(2).any(|w| w[0] == "--model" && w[1] == model), "{harness}: {args:?}");
        assert!(!args.iter().any(|a| a == "--profile"), "{harness}: {args:?}");
    }
}
