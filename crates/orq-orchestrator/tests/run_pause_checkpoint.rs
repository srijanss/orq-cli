use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{latest_checkpoint, record_checkpoint, Checkpoint, RunRecord, RunStore};

fn store(tag: &str) -> (RunStore, RunRecord) {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-pause-cp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = RunStore::new(&root);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".into(),
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

fn checkpoint(head: &str) -> Checkpoint {
    Checkpoint {
        completed_stages: vec!["implement".into(), "gate".into()],
        next_stage: Some("rebase".into()),
        branch: "fix--COM-02".into(),
        worktree: PathBuf::from("/repo/.claude/worktrees/fix--COM-02"),
        head_sha: head.into(),
        push_ref: None,
        review_request_ref: None,
    }
}

#[test]
fn a_checkpoint_is_stored_with_the_run_and_read_back_unchanged() {
    let (store, _) = store("roundtrip");
    assert_eq!(latest_checkpoint(&store, "run-1").unwrap(), None);
    let mut cp = checkpoint(&"b".repeat(40));
    cp.push_ref = Some("refs/heads/fix--COM-02".into());
    cp.review_request_ref = Some("https://github.example/o/r/pull/7".into());
    record_checkpoint(&store, "run-1", &cp).unwrap();
    assert_eq!(latest_checkpoint(&store, "run-1").unwrap(), Some(cp));
}

#[test]
fn the_latest_checkpoint_wins_and_an_unknown_run_is_an_error() {
    let (store, _) = store("latest");
    record_checkpoint(&store, "run-1", &checkpoint(&"b".repeat(40))).unwrap();
    record_checkpoint(&store, "run-1", &checkpoint(&"c".repeat(40))).unwrap();
    assert_eq!(
        latest_checkpoint(&store, "run-1")
            .unwrap()
            .unwrap()
            .head_sha,
        "c".repeat(40)
    );
    assert!(record_checkpoint(&store, "nope", &checkpoint("x")).is_err());
}
