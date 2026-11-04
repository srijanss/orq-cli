use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{read_handoff, write_handoff, HandoffDiff, PlanItem, RunRecord, RunStore, HANDOFF_FILE};

fn store(tag: &str) -> (RunStore, RunRecord) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-handoff-read-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    let record = RunRecord {
        run_id: "run-2".into(),
        project_id: "repo-1".into(),
        feature_id: "COM-02".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch: "fix--COM-02".into(),
        worktree: PathBuf::from("/repo/.claude/worktrees/fix--COM-02"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    (store, record)
}

#[test]
fn a_written_handoff_is_read_back_unchanged_and_a_run_without_one_has_none() {
    let (store, record) = store("roundtrip");
    assert_eq!(read_handoff(&store, "run-2").unwrap(), None);

    let plan = vec![PlanItem { id: "COM-02".into(), description: None, status: "pending".into() }];
    let diff = HandoffDiff { base_sha: "a".repeat(40), head_sha: "b".repeat(40), files: Vec::new() };
    write_handoff(&store, &record, "run-2", plan.clone(), diff).unwrap();
    let handoff = read_handoff(&store, "run-2").unwrap().expect("the handoff");
    assert_eq!(handoff.plan, plan);
    assert_eq!(handoff.run_id, "run-2");

    // A file that is not a handoff is an error, not "no handoff".
    store.write_run_file("run-2", HANDOFF_FILE, b"{not json").unwrap();
    assert!(read_handoff(&store, "run-2").is_err());
}
