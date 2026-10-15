//! MVP 1.1 demo: create a feature by hand and from a spec with a fake
//! agent in a disposable repository, promote it, and read the result the way
//! the TDD MCP server does. Run: `cargo test -p orq-features --test mvp11_end_to_end_demo`.
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{
    extract_candidates, review_candidates, run_generation, AgentProfile, FeatureDraft, GenerationRequest,
    LoadedLedger, Verdict,
};
use orq_test_support::FakeAgent;
use serde_json::Value;

fn disposable_repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-mvp11-demo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SPEC.md"), "# Demo spec\nA tiny app.\n").unwrap();
    dir
}

/// What the TDD MCP server needs from a feature: string `featureName`, string
/// `status`, and `dependsOn` as strings when present.
fn mcp_view(path: &PathBuf) -> Vec<(String, String, Vec<String>)> {
    let items: Vec<Value> = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    items
        .iter()
        .map(|item| {
            let deps = item
                .get("dependsOn")
                .map(|d| d.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect())
                .unwrap_or_default();
            (
                item["featureName"].as_str().unwrap().to_string(),
                item["status"].as_str().unwrap().to_string(),
                deps,
            )
        })
        .collect()
}

#[test]
fn a_feature_is_created_manually_and_from_a_spec_then_promoted_and_read_by_the_mcp() {
    let repo = disposable_repo();
    let ledger = repo.join(".tdd-features.json");

    // 1. Manual creation into an absent file.
    let mut manual = FeatureDraft::new("login");
    manual.description = Some("Sign in".into());
    LoadedLedger::load_or_new(&ledger).unwrap().append(&manual).unwrap();

    // 2. Spec-assisted generation with a fake agent.
    let output = r#"[{"featureName":"logout","dependsOn":["login"],"parallelizable":true,"status":"completed"},
                     {"featureName":"login"},{"featureName":"orphan","dependsOn":["ghost"]}]"#;
    let agent = FakeAgent::builder().stdout(output).build();
    let profile = AgentProfile { harness: "claude".into(), profile: "writer".into() };
    let artifacts = repo.join("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let artifact = run_generation(&GenerationRequest {
        program: agent.path(),
        profile: &profile,
        repo_root: &repo,
        spec: &repo.join("SPEC.md"),
        artifact_dir: &artifacts,
        timeout: Duration::from_secs(10),
    })
    .unwrap();
    let candidates = extract_candidates(&fs::read_to_string(artifact).unwrap()).unwrap();
    let loaded = LoadedLedger::load(&ledger).unwrap();
    let reviewed = review_candidates(loaded.entries(), candidates);
    assert_eq!(reviewed[0].verdict, Verdict::New);
    assert_eq!(reviewed[1].verdict, Verdict::DuplicateOfExisting);
    assert!(matches!(reviewed[2].verdict, Verdict::Invalid(_)));
    let accepted: Vec<FeatureDraft> =
        reviewed.iter().filter(|r| r.verdict == Verdict::New).map(|r| r.candidate.draft.clone()).collect();
    loaded.append_many(&accepted).unwrap();

    // 3. Everything starts as a draft; promotion makes it schedulable.
    assert_eq!(
        mcp_view(&ledger),
        vec![
            ("login".into(), "draft".into(), vec![]),
            ("logout".into(), "draft".into(), vec!["login".into()]),
        ]
    );
    let loaded = LoadedLedger::load(&ledger).unwrap();
    loaded.promote("login", None).unwrap();

    let view = mcp_view(&ledger);
    assert_eq!(view[0].1, "pending");
    assert_eq!(view[1].1, "draft");
    let text = fs::read_to_string(&ledger).unwrap();
    assert!(!text.contains("recordedAt") && !text.contains("cyclesCompleted"));
    let _ = fs::remove_dir_all(&repo);
}
