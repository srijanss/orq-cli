use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{
    run_gate_stage_with_env, run_setup_with_env, Gate, GateOutcome, RunRecord, RunStore,
};

fn dir(tag: &str) -> PathBuf {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-step-env-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("worktree")).unwrap();
    root
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

fn env() -> Vec<(String, String)> {
    vec![("DB_NAME".to_string(), "shop_fix_com_01".to_string())]
}

#[test]
fn a_setup_step_sees_the_worktrees_database_names() {
    let root = dir("setup");
    let worktree = root.join("worktree");

    run_setup_with_env(
        &worktree,
        &[sh("printf %s \"$DB_NAME\" > seen.txt")],
        &root.join("setup.log"),
        &env(),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(worktree.join("seen.txt")).unwrap(),
        "shop_fix_com_01"
    );
}

#[test]
fn a_gate_step_sees_the_worktrees_database_names_and_still_nothing_else_of_the_callers_environment()
{
    let root = dir("gate");
    let worktree = root.join("worktree");
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-01".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-01".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let gate = Gate::Steps {
        steps: vec![sh("printf %s \"$DB_NAME|${CARGO_HOME-unset}\" > seen.txt")],
        notes: Vec::new(),
    };
    std::env::set_var("CARGO_HOME", "/must-not-leak");

    let outcome =
        run_gate_stage_with_env(&store, "run-1", &gate, true, &root.join("gate.log"), &env())
            .unwrap();

    assert_eq!(outcome, GateOutcome::Passed);
    assert_eq!(
        fs::read_to_string(worktree.join("seen.txt")).unwrap(),
        "shop_fix_com_01|unset"
    );
}
