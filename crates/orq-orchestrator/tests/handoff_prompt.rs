use std::path::PathBuf;

use orq_orchestrator::{handoff_prompt, DiffFile, GateResult, Handoff, HandoffDiff, PlanItem};

fn handoff() -> Handoff {
    Handoff {
        schema_version: 1,
        from_run: "run-1".to_string(),
        run_id: "run-2".to_string(),
        feature_id: "COM-01".to_string(),
        branch: "fix--COM-01".to_string(),
        worktree: PathBuf::from("/work/COM-01"),
        plan: vec![
            PlanItem { id: "COM-01".to_string(), description: Some("Add the widget".to_string()), status: "in_progress".to_string() },
            PlanItem { id: "COM-02".to_string(), description: None, status: "pending".to_string() },
        ],
        diff: HandoffDiff {
            base_sha: "aaaa".to_string(),
            head_sha: "bbbb".to_string(),
            files: vec![DiffFile { path: "src/widget.rs".to_string(), old_path: None, added: Some(12), deleted: Some(3) }],
        },
        gate: GateResult { outcome: "failed".to_string(), reason: Some("2 tests failed".to_string()), exit_code: Some(101), log: Some("/logs/gate.log".to_string()) },
        completed_stages: vec!["implement_and_review".to_string()],
        next_stage: Some("gate".to_string()),
    }
}

#[test]
fn the_handoff_prompt_states_the_plan_the_diff_the_test_results_and_what_remains_without_a_conversation() {
    let prompt = handoff_prompt(&handoff());
    assert!(prompt.contains("COM-01 (in_progress): Add the widget"), "{prompt}");
    assert!(prompt.contains("COM-02 (pending)"), "{prompt}");
    assert!(prompt.contains("src/widget.rs (+12 -3)"), "{prompt}");
    assert!(prompt.contains("aaaa..bbbb"), "{prompt}");
    assert!(prompt.contains("failed") && prompt.contains("2 tests failed") && prompt.contains("/logs/gate.log"), "{prompt}");
    assert!(prompt.contains("fix--COM-01") && prompt.contains("/work/COM-01"), "{prompt}");
    assert!(prompt.contains("Completed stages: implement_and_review"), "{prompt}");
    assert!(prompt.contains("Remaining: gate"), "{prompt}");
    assert!(prompt.contains("no access to the previous session"), "{prompt}");
    assert!(!prompt.contains("run-1"), "the previous run's id is no use to the new session: {prompt}");
}
