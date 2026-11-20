//! The evidence a run's events hold for a head is that head's: its own gate and its own CI, even when the run
//! pushed again after it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{recorded_evidence, run_gate_stage, Gate, RunRecord, RunStore};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn world() -> (RunStore, PathBuf, PathBuf) {
    world_at("own-head")
}

fn world_at(tag: &str) -> (RunStore, PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-report-evidence-{}-{tag}", std::process::id()));
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
    Gate::Steps {
        steps: vec![vec!["sh".into(), "-c".into(), script.into()]],
        notes: Vec::new(),
    }
}

#[test]
fn each_head_gets_its_own_gate_even_after_a_later_gate_ran() {
    let (store, worktree, log) = world();
    let first = commit(&worktree, "first");
    run_gate_stage(&store, "run-1", &gate("true"), true, &log).unwrap();
    let second = commit(&worktree, "second");
    run_gate_stage(&store, "run-1", &gate("exit 3"), true, &log).unwrap();

    let earlier = recorded_evidence(&store, "run-1", &first).unwrap();
    assert!(
        earlier.gate_passed,
        "the first head passed its gate: {earlier:?}"
    );
    assert_eq!(
        earlier.gate_steps,
        Some(vec![vec!["sh".to_string(), "-c".into(), "true".into()]])
    );

    let later = recorded_evidence(&store, "run-1", &second).unwrap();
    assert!(
        !later.gate_passed,
        "the second head failed its own: {later:?}"
    );
    assert_eq!(later.gate_steps, None);

    let unseen = recorded_evidence(&store, "run-1", &"f".repeat(40)).unwrap();
    assert!(
        !unseen.gate_passed,
        "a head no gate ran on has no gate evidence: {unseen:?}"
    );
}

#[test]
fn a_gate_whose_commit_could_not_be_read_is_no_heads_evidence() {
    let (store, worktree, log) = world_at("unknown-head");
    let head = commit(&worktree, "first");
    run_gate_stage(&store, "run-1", &gate("true"), true, &log).unwrap();
    // git cannot say what is checked out now: later gates record no commit.
    std::fs::rename(worktree.join(".git"), worktree.join("git-away")).unwrap();
    run_gate_stage(&store, "run-1", &gate("exit 3"), true, &log).unwrap();
    assert!(
        recorded_evidence(&store, "run-1", &head)
            .unwrap()
            .gate_passed,
        "a gate of an unknown commit does not undo the head's own"
    );

    run_gate_stage(&store, "run-1", &gate("true"), true, &log).unwrap();
    assert!(
        !recorded_evidence(&store, "run-1", &"f".repeat(40))
            .unwrap()
            .gate_passed,
        "nor vouches for another head"
    );
}

const CI_HEAD: &str = "c1c1000000000000000000000000000000000001";

/// Records a CI stage event of `kind` with `payload` on run-1.
fn ci_event(store: &RunStore, kind: &str, payload: serde_json::Value) {
    let n = store.events("run-1").unwrap().len();
    store
        .append_event(&orq_orchestrator::Event {
            schema_version: 1,
            event_id: format!("ci-{n}"),
            run_id: "run-1".into(),
            project_id: "repo-1".into(),
            feature_id: "f".into(),
            kind: kind.into(),
            stage: Some("ci".into()),
            attempt: 1,
            occurred_at: "2026-11-18T09:00:00Z".into(),
            payload,
        })
        .unwrap();
}

#[test]
fn a_newer_ci_failure_that_cannot_be_read_is_not_covered_by_an_older_pass() {
    use orq_orchestrator::CiOutcome;
    use serde_json::json;
    for unreadable in [
        json!({"head_sha": CI_HEAD}),
        json!({"result": "exploded", "head_sha": CI_HEAD}),
        json!({"result": "failed", "head_sha": CI_HEAD}),
    ] {
        let (store, _, _) = world_at("ci-unreadable");
        ci_event(
            &store,
            "stage.completed",
            json!({"result": "passed", "head_sha": CI_HEAD}),
        );
        ci_event(&store, "stage.failed", unreadable.clone());

        let ci = recorded_evidence(&store, "run-1", CI_HEAD).unwrap().ci;
        assert!(
            matches!(
                ci,
                Some(CiOutcome::Failed { .. } | CiOutcome::Errored { .. })
            ),
            "{unreadable} after a pass gave {ci:?}"
        );
    }
}

#[test]
fn a_ci_failure_that_names_no_failing_checks_is_still_a_failure() {
    let (store, _, _) = world_at("ci-failed-unnamed");
    ci_event(
        &store,
        "stage.failed",
        serde_json::json!({"result": "failed", "head_sha": CI_HEAD}),
    );

    let ci = recorded_evidence(&store, "run-1", CI_HEAD).unwrap().ci;
    assert_eq!(
        ci,
        Some(orq_orchestrator::CiOutcome::Failed {
            failing: Vec::new()
        })
    );
}

#[test]
fn each_ci_result_for_the_head_is_its_evidence_and_another_heads_is_not() {
    use orq_orchestrator::CiOutcome;
    use serde_json::json;
    let other = "0e0e000000000000000000000000000000000002";
    let cases = [
        (
            "stage.completed",
            json!({"result": "passed", "head_sha": CI_HEAD}),
            Some(CiOutcome::Passed),
        ),
        (
            "stage.failed",
            json!({"result": "failed", "failing": ["lint"], "head_sha": CI_HEAD}),
            Some(CiOutcome::Failed {
                failing: vec!["lint".into()],
            }),
        ),
        (
            "stage.failed",
            json!({"result": "timed_out", "head_sha": CI_HEAD}),
            Some(CiOutcome::TimedOut),
        ),
        (
            "stage.failed",
            json!({"result": "errored", "reason": "no token", "head_sha": CI_HEAD}),
            Some(CiOutcome::Errored {
                reason: "no token".into(),
            }),
        ),
        // CI disabled: a skip names no head and holds for any.
        (
            "stage.skipped",
            json!({"reason": "disabled"}),
            Some(CiOutcome::Skipped),
        ),
        // Another head's result, and a pass that names no head, say nothing about this one.
        (
            "stage.completed",
            json!({"result": "passed", "head_sha": other}),
            None,
        ),
        ("stage.completed", json!({"result": "passed"}), None),
    ];
    for (kind, payload, expected) in cases {
        let (store, _, _) = world_at("ci-table");
        ci_event(&store, kind, payload.clone());
        assert_eq!(
            recorded_evidence(&store, "run-1", CI_HEAD).unwrap().ci,
            expected,
            "{kind} {payload}"
        );
    }

    // With results for two heads, each keeps its own, whichever came last.
    let (store, _, _) = world_at("ci-two-heads");
    ci_event(
        &store,
        "stage.failed",
        json!({"result": "failed", "failing": [], "head_sha": CI_HEAD}),
    );
    ci_event(
        &store,
        "stage.completed",
        json!({"result": "passed", "head_sha": other}),
    );
    assert_eq!(
        recorded_evidence(&store, "run-1", CI_HEAD).unwrap().ci,
        Some(CiOutcome::Failed {
            failing: Vec::new()
        })
    );
    assert_eq!(
        recorded_evidence(&store, "run-1", other).unwrap().ci,
        Some(CiOutcome::Passed)
    );
}
