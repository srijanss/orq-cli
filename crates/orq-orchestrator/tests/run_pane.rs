use std::collections::BTreeMap;
use std::path::PathBuf;

use orq_orchestrator::{list_runs, record_pane, run_pane, PaneRef, RunRecord, RunStore};

fn store(tag: &str) -> RunStore {
    let root = orq_test_support::process_temp_dir().join(format!("orq-run-pane-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    RunStore::new(&root)
}

fn record(run_id: &str) -> RunRecord {
    RunRecord {
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-01".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "abc".to_string(),
        branch: "fix--COM-01".to_string(),
        worktree: PathBuf::from("/tmp/w"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn pane(tab: &str) -> PaneRef {
    PaneRef { workspace_id: "w1".into(), tab_id: tab.into(), pane_id: format!("{tab}:p1") }
}

#[test]
fn a_recorded_pane_is_read_back_and_the_latest_one_wins() {
    let s = store("latest");
    s.create(&record("run-1")).unwrap();
    assert_eq!(run_pane(&s, "run-1").unwrap(), None);

    record_pane(&s, "run-1", &pane("w1:t2")).unwrap();
    assert_eq!(run_pane(&s, "run-1").unwrap(), Some(pane("w1:t2")));

    record_pane(&s, "run-1", &pane("w1:t3")).unwrap();
    assert_eq!(run_pane(&s, "run-1").unwrap(), Some(pane("w1:t3")));
}

#[test]
fn recording_a_pane_does_not_change_how_the_run_lists() {
    let s = store("lists");
    s.create(&record("run-1")).unwrap();
    let before = list_runs(&s, "repo-1").unwrap();

    record_pane(&s, "run-1", &pane("w1:t2")).unwrap();

    assert_eq!(list_runs(&s, "repo-1").unwrap(), before);
    assert!(record_pane(&s, "no-such-run", &pane("w1:t2")).is_err());
}
