//! The evidence a run's events hold for a head is that head's: its own gate and its own CI, even when the run
//! pushed again after it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{recorded_evidence, run_gate_stage, Gate, RunRecord, RunStore};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.email=t@example.com", "-c", "user.name=t"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn world() -> (RunStore, PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-report-evidence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q", "-b", "fix--f"]);
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "f".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "0".repeat(40),
            branch: "fix--f".into(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    (store, worktree, root.join("gate.log"))
}

fn gate(script: &str) -> Gate {
    Gate::Steps { steps: vec![vec!["sh".into(), "-c".into(), script.into()]], notes: Vec::new() }
}

#[test]
fn each_head_gets_its_own_gate_even_after_a_later_gate_ran() {
    let (store, worktree, log) = world();
    let first = commit(&worktree, "first");
    run_gate_stage(&store, "run-1", &gate("true"), true, &log).unwrap();
    let second = commit(&worktree, "second");
    run_gate_stage(&store, "run-1", &gate("exit 3"), true, &log).unwrap();

    let earlier = recorded_evidence(&store, "run-1", &first).unwrap();
    assert!(earlier.gate_passed, "the first head passed its gate: {earlier:?}");
    assert_eq!(earlier.gate_steps, Some(vec![vec!["sh".to_string(), "-c".into(), "true".into()]]));

    let later = recorded_evidence(&store, "run-1", &second).unwrap();
    assert!(!later.gate_passed, "the second head failed its own: {later:?}");
    assert_eq!(later.gate_steps, None);

    let unseen = recorded_evidence(&store, "run-1", &"f".repeat(40)).unwrap();
    assert!(!unseen.gate_passed, "a head no gate ran on has no gate evidence: {unseen:?}");
}
