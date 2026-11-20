#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use orq_agents::{adapter_for, ImplementRequest, ModelSpec};
use orq_core::FeatureStatus;
use orq_features::{in_implement_step, load_ledger, promote_feature, reopen_completed_feature, reopen_open_findings, LedgerStatus, PromoteError, FINDING_REOPENED};

const LEDGER: &str = "[\n  {\"featureName\": \"run-feature\", \"status\": \"in_progress\"},\n  {\"featureName\": \"fix-leak\", \"status\": \"completed\", \"recordedAt\": \"2026-11-20T10:00:00Z\"},\n  {\"featureName\": \"other\", \"status\": \"completed\"}\n]\n";

/// A scratch project root holding `LEDGER` and a worktree beside it holding the run's review ledger.
fn scratch(label: &str, findings: &str, rounds: &str) -> (PathBuf, PathBuf) {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-reopen-finding-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let worktree = dir.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let ledger = dir.join(".tdd-features.json");
    fs::write(&ledger, LEDGER).unwrap();
    fs::write(worktree.join(".tdd-review-findings.json"), findings).unwrap();
    fs::write(worktree.join(".tdd-review-rounds.json"), rounds).unwrap();
    (ledger, worktree)
}

fn status(ledger: &PathBuf, name: &str) -> LedgerStatus {
    load_ledger(ledger).unwrap().into_iter().find(|e| e.feature_name == name).unwrap().status
}

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn prompt(reopened: &[&str]) -> String {
    let request = ImplementRequest {
        features: &["run-feature"],
        implementer: spec("claude", "opus"),
        reviewer: spec("pi", "kimi-k3"),
        context: None,
        exit_marker: None,
        attended: false,
        unattended: true,
        remote_control: None,
        handoff: None,
        continuing: None,
        review_again: None,
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
        declared_files: &[],
        undeclared_files: &[],
        feature_specs: &[],
        answer: None,
        reopened_findings: reopened,
    };
    adapter_for("claude").unwrap().implement_argv(&request).unwrap().join("\n")
}

const OPEN_FIX_LEAK: &str = r#"{"review:abc1234": [{"id": "fix-leak", "severity": "medium", "claim": "leaks", "status": "open"}]}"#;
const ROUNDS: &str = r#"{"review:abc1234": [{"round": 1, "start": "abc1234", "head": "def5678", "findings": [{"id": "fix-leak", "status": "fixed"}]}, {"round": 2, "start": "abc1234", "head": "fed8765", "findings": [{"id": "fix-leak", "status": "open"}]}]}"#;

#[test]
fn a_completed_feature_with_an_open_finding_of_its_id_is_set_pending_recorded_and_named_in_the_prompt() {
    let (ledger, worktree) = scratch("open", OPEN_FIX_LEAK, ROUNDS);
    let mut events = Vec::new();

    let reopened = reopen_open_findings(&ledger, &worktree, &mut |kind, payload| events.push((kind.to_string(), payload))).unwrap();

    assert_eq!(status(&ledger, "fix-leak"), LedgerStatus::Known(orq_core::FeatureStatus::Pending));
    assert_eq!(status(&ledger, "other"), LedgerStatus::Known(orq_core::FeatureStatus::Completed));
    assert_eq!(FINDING_REOPENED, "ledger.finding_reopened");
    assert_eq!(events, vec![(FINDING_REOPENED.to_string(), serde_json::json!({"feature": "fix-leak", "finding": "fix-leak", "round": 2}))]);
    let names: Vec<&str> = reopened.iter().map(|r| r.finding.as_str()).collect();
    assert_eq!(names, ["fix-leak"]);

    let text = prompt(&names);
    assert!(text.contains("fix-leak was reopened"), "{text}");
    assert!(text.contains("init_feature(reviewFindingId=...)"), "{text}");
    assert!(!prompt(&[]).contains("was reopened"));
}

#[test]
fn a_completed_feature_with_no_open_finding_stays_completed() {
    let findings = r#"{"review:abc1234": [{"id": "fix-leak", "severity": "medium", "claim": "leaks", "status": "rejected"}, {"id": "nothing-in-ledger", "status": "open"}]}"#;
    let (ledger, worktree) = scratch("closed", findings, ROUNDS);
    let mut events = Vec::new();

    let reopened = reopen_open_findings(&ledger, &worktree, &mut |kind, payload| events.push((kind.to_string(), payload))).unwrap();

    assert!(reopened.is_empty());
    assert!(events.is_empty());
    assert_eq!(fs::read_to_string(&ledger).unwrap(), LEDGER);
}

#[test]
fn completed_to_pending_is_refused_without_reopen_and_allowed_with_it() {
    let (ledger, _) = scratch("by-hand", "{}", "{}");

    let refused = promote_feature(&ledger, "fix-leak");
    assert!(matches!(refused, Err(PromoteError::IllegalTransition(_))), "{refused:?}");
    assert_eq!(fs::read_to_string(&ledger).unwrap(), LEDGER);

    let entry = reopen_completed_feature(&ledger, "fix-leak", false).unwrap();
    assert_eq!(entry.status, LedgerStatus::Known(FeatureStatus::Pending));
    assert_eq!(status(&ledger, "fix-leak"), LedgerStatus::Known(FeatureStatus::Pending));
    assert_eq!(status(&ledger, "other"), LedgerStatus::Known(FeatureStatus::Completed));
}

#[test]
fn reopen_is_refused_while_a_run_of_the_project_is_in_its_implement_step() {
    let (ledger, _) = scratch("implementing", "{}", "{}");
    let implementing = [("run.launched", None), ("stage.started", Some("implement_and_review"))];
    let finished = [("stage.started", Some("implement_and_review")), ("stage.completed", Some("implement_and_review")), ("stage.started", Some("gate"))];
    let ended = [("stage.started", Some("implement_and_review")), ("run.failed", None)];

    assert!(in_implement_step(implementing));
    assert!(!in_implement_step(finished));
    assert!(!in_implement_step(ended));

    let refused = reopen_completed_feature(&ledger, "fix-leak", in_implement_step(implementing));
    assert!(matches!(refused, Err(PromoteError::Implementing)), "{refused:?}");
    assert_eq!(fs::read_to_string(&ledger).unwrap(), LEDGER);

    reopen_completed_feature(&ledger, "fix-leak", in_implement_step(finished)).unwrap();
    assert_eq!(status(&ledger, "fix-leak"), LedgerStatus::Known(FeatureStatus::Pending));
}

#[test]
fn only_a_completed_feature_is_reopened_by_hand() {
    let (ledger, _) = scratch("only-completed", "{}", "{}");
    let in_progress = reopen_completed_feature(&ledger, "run-feature", false);
    assert!(matches!(in_progress, Err(PromoteError::IllegalTransition(_))), "{in_progress:?}");
}
