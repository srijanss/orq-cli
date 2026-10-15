use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use orq_features::{run_generation_cancellable, AgentProfile, GenerationError, GenerationRequest};

#[test]
fn a_flag_already_set_never_starts_the_agent() {
    let root = std::env::temp_dir().join(format!("orq-gencancel-early-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let spec = root.join("SPEC.md");
    fs::write(&spec, "x").unwrap();
    // A program that cannot start: spawning it would report AgentNotFound, so
    // `Cancelled` can only come from checking the flag before the spawn.
    let missing = root.join("no-such-harness");
    let model = AgentProfile { harness: "claude".into(), model: "writer".into() };

    let result = run_generation_cancellable(
        &GenerationRequest {
            program: &missing,
            model: &model,
            repo_root: &root,
            spec: &spec,
            artifact_dir: &root.join("out"),
            timeout: Duration::from_secs(10),
            focus: None,
        },
        &AtomicBool::new(true),
    );

    assert!(matches!(result, Err(GenerationError::Cancelled)), "{result:?}");
}
