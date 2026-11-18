//! What a run is doing right now, read off its stored events. Pure: the caller loads
//! the events, so listing many runs and testing the rules need no process or disk.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

/// Record that the run has ended. `ending` is `review_ready`, `blocked`, `needs_review` or
/// `failed`; a reason is kept for the last three. Listing reads this instead of inferring the end from other events.
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
    /// The implement session's review escalated: the run waits for a human to review again,
    /// accept or discard its work.
    NeedsReview,
    /// A pause was asked for; the run keeps going until it reaches a stage boundary.
    PauseRequested,
    /// The run stopped at a stage boundary and can be resumed.
    Paused,
    /// A failed or review-waiting run whose commits were all merged into its target branch by
    /// hand. Never read off the events: only [`crate::mark_merged_runs`] says so, from git.
    Merged,
    /// A failed or review-waiting run whose tests are running so it can be accepted: `run.accept_started`
    /// with no result after it.
    Accepting,
}

impl RunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RunPhase::Running => "running",
            RunPhase::Completed => "completed",
            RunPhase::Failed => "failed",
            RunPhase::Blocked => "blocked",
            RunPhase::NeedsReview => "needs_review",
            RunPhase::PauseRequested => "pause_requested",
            RunPhase::Paused => "paused",
            RunPhase::Merged => "merged",
            RunPhase::Accepting => "accepting",
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

/// How the run's MCP servers were set up: each server's version and where it came from, and the
/// notes on what did not go as planned. Notes never carry secret values.
pub fn record_mcp_setup(store: &RunStore, record: &RunRecord, report: &crate::McpSetupReport) -> Result<(), StoreError> {
    let servers: Vec<serde_json::Value> = report
        .versions
        .iter()
        .map(|s| serde_json::json!({ "name": s.name, "version": s.version, "source": s.source }))
        .collect();
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "mcp.setup".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "servers": servers, "notes": report.notes }),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub phase: RunPhase,
    /// The stage of the latest stage event, if any.
    pub stage: Option<String>,
    /// Why the run failed or is blocked, when the event said.
    pub reason: Option<String>,
    /// When the event that last set the phase happened, as that event wrote it.
    pub phase_at: Option<String>,
    /// The run this one was continued as, once a resume took it over.
    pub resumed_as: Option<String>,
    /// Why the latest accept failed, until another accept starts.
    pub accept_failure: Option<AcceptFailure>,
}

/// What the `run.accept_failed` event said: why, the end of the test output, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptFailure {
    pub reason: String,
    pub output: String,
    pub at: String,
}

/// Summarize a run from its events in order. The latest event that says something about the
/// run's state wins, so a stage that starts again after a failure is running again; unknown
/// event kinds are ignored so newer writers do not break older readers.
pub fn summarize_run(events: &[Event]) -> RunSummary {
    let mut summary = RunSummary {
        phase: RunPhase::Running,
        stage: None,
        reason: None,
        phase_at: None,
        resumed_as: None,
        accept_failure: None,
    };
    // What the run was before an accept started, given back when the accept fails.
    let mut before_accept: Option<(RunPhase, Option<String>, Option<String>)> = None;
    for event in events {
        let phase = match event.kind.as_str() {
            // A start with no result since the last one keeps what the run goes back to: it was accepting already.
            "run.accept_started" if summary.phase == RunPhase::Accepting => continue,
            "run.accept_started" => {
                before_accept = Some((summary.phase, summary.reason.take(), summary.phase_at.take()));
                summary.accept_failure = None;
                RunPhase::Accepting
            }
            "run.accept_failed" if summary.phase == RunPhase::Accepting => {
                if let Some((phase, reason, at)) = before_accept.take() {
                    (summary.phase, summary.reason, summary.phase_at) = (phase, reason, at);
                }
                let text = |key: &str| event.payload.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                summary.accept_failure = Some(AcceptFailure { reason: text("reason"), output: text("output"), at: event.occurred_at.clone() });
                continue;
            }
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
            // Whatever stopped it, a run resumed as another is handed over to that run: it is over
            // even if the `run.completed` that should follow was never written.
            "run.resumed" => match event.payload.get("resumed_as").and_then(|v| v.as_str()) {
                Some(id) => {
                    summary.resumed_as = Some(id.to_string());
                    RunPhase::Completed
                }
                None if summary.phase == RunPhase::Paused => RunPhase::Running,
                None => continue,
            },
            "stage.failed" | "run.failed" => RunPhase::Failed,
            "run.blocked" => RunPhase::Blocked,
            "worktree.removed" => RunPhase::Completed,
            // The explicit ending of the run: it wins over everything inferred before it.
            "run.completed" => match event.payload.get("ending").and_then(|e| e.as_str()) {
                Some("review_ready" | "resumed" | "pushed" | "merged") => RunPhase::Completed,
                Some("failed") => RunPhase::Failed,
                Some("blocked") => RunPhase::Blocked,
                Some("needs_review") => RunPhase::NeedsReview,
                _ => continue,
            },
            // Cleanup keeps the worktree of an unsuccessful run too: that says nothing about how
            // the run ended, so it never overrides a failure or a block, nor an unsuccessful run.
            "worktree.kept" => {
                let unsuccessful =
                    event.payload.get("run_succeeded").and_then(|v| v.as_bool()) == Some(false);
                if unsuccessful
                    || matches!(summary.phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview)
                {
                    continue;
                }
                RunPhase::Completed
            }
            _ => continue,
        };
        summary.phase = phase;
        summary.phase_at = Some(event.occurred_at.clone());
        summary.reason = match phase {
            RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview => event
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
