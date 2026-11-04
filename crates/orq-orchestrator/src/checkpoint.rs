//! The state a paused run is resumed from, kept in the run's own event log.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

const KIND: &str = "run.checkpoint";

/// Everything a resume needs to continue where the run stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub completed_stages: Vec<String>,
    pub next_stage: Option<String>,
    pub branch: String,
    pub worktree: PathBuf,
    pub head_sha: String,
    /// The pushed ref, once the push stage has run.
    pub push_ref: Option<String>,
    /// The review request (pull or merge request) reference, once one was opened.
    pub review_request_ref: Option<String>,
}

/// Appends `checkpoint` to the events of `run_id`.
pub fn record_checkpoint(store: &RunStore, run_id: &str, checkpoint: &Checkpoint) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: KIND.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::to_value(checkpoint).unwrap_or(serde_json::Value::Null),
    })
}

/// The most recent checkpoint of `run_id`, if one was recorded.
pub fn latest_checkpoint(store: &RunStore, run_id: &str) -> Result<Option<Checkpoint>, StoreError> {
    let events = store.events(run_id)?;
    Ok(events.iter().rev().find(|e| e.kind == KIND).and_then(|e| serde_json::from_value(e.payload.clone()).ok()))
}

/// Called by the runner between stages. When a pause was requested, records a checkpoint (the stages
/// done so far, the stage that would run next, and where the branch stands) and the `run.paused`
/// event, and returns true: the runner then stops without ending the run. Otherwise does nothing.
pub fn pause_at_boundary(
    store: &RunStore,
    run_id: &str,
    completed_stages: &[&str],
    next_stage: &str,
    push_ref: Option<&str>,
    review_request_ref: Option<&str>,
) -> Result<bool, StoreError> {
    let events = store.events(run_id)?;
    if crate::run_status::summarize_run(&events).phase != crate::run_status::RunPhase::PauseRequested {
        return Ok(false);
    }
    let record = store.load(run_id)?;
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&record.worktree)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(StoreError::from)?;
    if !head.status.success() {
        return Err(StoreError::Corrupt(format!("cannot read HEAD of {}", record.worktree.display())));
    }
    let checkpoint = Checkpoint {
        completed_stages: completed_stages.iter().map(|s| s.to_string()).collect(),
        next_stage: Some(next_stage.to_string()),
        branch: record.branch.clone(),
        worktree: record.worktree.clone(),
        head_sha: String::from_utf8_lossy(&head.stdout).trim().to_string(),
        push_ref: push_ref.map(str::to_string),
        review_request_ref: review_request_ref.map(str::to_string),
    };
    record_checkpoint(store, run_id, &checkpoint)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: "run.paused".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "next_stage": next_stage }),
    })?;
    Ok(true)
}
