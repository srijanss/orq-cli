//! What a run is doing right now, read off its stored events. Pure: the caller loads
//! the events, so listing many runs and testing the rules need no process or disk.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

/// Record that the run has ended. `ending` is `review_ready`, `blocked` or `failed`; a reason
/// is kept for the last two. Listing reads this instead of inferring the end from other events.
pub fn record_run_completed(
    store: &RunStore,
    record: &RunRecord,
    ending: &str,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let mut payload = serde_json::json!({ "ending": ending });
    if let Some(reason) = reason {
        payload["reason"] = serde_json::Value::String(reason.to_string());
    }
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "run.completed".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// Started or between stages; nothing has ended it.
    Running,
    /// The run reached its end (the worktree was cleaned up or deliberately kept).
    Completed,
    Failed,
    /// Waiting on a human, for example a rebase conflict.
    Blocked,
    /// A pause was asked for; the run keeps going until it reaches a stage boundary.
    PauseRequested,
    /// The run stopped at a stage boundary and can be resumed.
    Paused,
}

impl RunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RunPhase::Running => "running",
            RunPhase::Completed => "completed",
            RunPhase::Failed => "failed",
            RunPhase::Blocked => "blocked",
            RunPhase::PauseRequested => "pause_requested",
            RunPhase::Paused => "paused",
        }
    }
}

/// Ask a running run to pause at its next stage boundary. Refused for any other phase, so a
/// finished run is never marked as pausing.
pub fn request_pause(store: &RunStore, run_id: &str) -> Result<(), String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    match phase {
        RunPhase::Running => {}
        RunPhase::PauseRequested | RunPhase::Paused => {
            return Err(format!("{run_id} is already {}", phase.as_str()));
        }
        other => {
            return Err(format!(
                "{run_id} is {}; only a running run can be paused",
                other.as_str()
            ))
        }
    }
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(run_id),
            run_id: record.run_id,
            project_id: record.project_id,
            feature_id: record.feature_id,
            kind: "run.pause_requested".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload: serde_json::json!({}),
        })
        .map_err(|e| e.to_string())
}

/// Record that a paused run's `role` should continue on `profile`. Only a paused run, which sits at
/// a safe checkpoint, accepts the request; any other run is refused and nothing is recorded.
pub fn request_model_switch(
    store: &RunStore,
    run_id: &str,
    role: &str,
    profile: &str,
) -> Result<(), String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if phase != RunPhase::Paused {
        return Err(format!(
            "{run_id} is {}; only a paused run can switch models",
            phase.as_str()
        ));
    }
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(run_id),
            run_id: record.run_id,
            project_id: record.project_id,
            feature_id: record.feature_id,
            kind: "run.model_switch_requested".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload: serde_json::json!({ "role": role, "profile": profile }),
        })
        .map_err(|e| e.to_string())
}

/// Record that `role` of a resumed run continues on another profile than the paused run's.
pub fn record_model_switched(
    store: &RunStore,
    record: &RunRecord,
    role: &str,
    from: &crate::store::RoleProfile,
    to: &crate::store::RoleProfile,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "run.model_switched".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "role": role, "from": from, "to": to }),
    })
}

/// A stage needed `capability` and `harness` does not have it, so the stage went on without it.
pub fn record_capability_skipped(
    store: &RunStore,
    record: &RunRecord,
    stage: &str,
    harness: &str,
    capability: &str,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "capability.skipped".to_string(),
        stage: Some(stage.to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "harness": harness, "capability": capability }),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub phase: RunPhase,
    /// The stage of the latest stage event, if any.
    pub stage: Option<String>,
    /// Why the run failed or is blocked, when the event said.
    pub reason: Option<String>,
}

/// Summarize a run from its events in order. The latest event that says something about the
/// run's state wins, so a stage that starts again after a failure is running again; unknown
/// event kinds are ignored so newer writers do not break older readers.
pub fn summarize_run(events: &[Event]) -> RunSummary {
    let mut summary = RunSummary {
        phase: RunPhase::Running,
        stage: None,
        reason: None,
    };
    for event in events {
        let phase = match event.kind.as_str() {
            // A requested pause or a pause itself outlives the stages that finish around it.
            "stage.started" | "stage.completed" | "stage.skipped"
                if matches!(summary.phase, RunPhase::PauseRequested | RunPhase::Paused) =>
            {
                summary.stage = event.stage.clone();
                continue;
            }
            "stage.started" | "stage.completed" | "stage.skipped" => RunPhase::Running,
            "run.pause_requested" if summary.phase == RunPhase::Running => RunPhase::PauseRequested,
            "run.paused" if summary.phase == RunPhase::PauseRequested => RunPhase::Paused,
            "run.resumed" if summary.phase == RunPhase::Paused => RunPhase::Running,
            "stage.failed" | "run.failed" => RunPhase::Failed,
            "run.blocked" => RunPhase::Blocked,
            "worktree.removed" => RunPhase::Completed,
            // The explicit ending of the run: it wins over everything inferred before it.
            "run.completed" => match event.payload.get("ending").and_then(|e| e.as_str()) {
                Some("review_ready" | "resumed") => RunPhase::Completed,
                Some("failed") => RunPhase::Failed,
                Some("blocked") => RunPhase::Blocked,
                _ => continue,
            },
            // Cleanup keeps the worktree of an unsuccessful run too: that says nothing about how
            // the run ended, so it never overrides a failure or a block, nor an unsuccessful run.
            "worktree.kept" => {
                let unsuccessful =
                    event.payload.get("run_succeeded").and_then(|v| v.as_bool()) == Some(false);
                if unsuccessful || matches!(summary.phase, RunPhase::Failed | RunPhase::Blocked) {
                    continue;
                }
                RunPhase::Completed
            }
            _ => continue,
        };
        summary.phase = phase;
        summary.reason = match phase {
            RunPhase::Failed | RunPhase::Blocked => event
                .payload
                .get("reason")
                .and_then(|r| r.as_str())
                .map(str::to_string),
            _ => None,
        };
        if event.kind.starts_with("stage.") {
            summary.stage = event.stage.clone();
        } else if matches!(phase, RunPhase::Failed | RunPhase::Blocked) && event.stage.is_some() {
            summary.stage = event.stage.clone();
        }
    }
    summary
}
