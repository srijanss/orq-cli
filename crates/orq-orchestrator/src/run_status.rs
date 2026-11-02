//! What a run is doing right now, read off its stored events. Pure: the caller loads
//! the events, so listing many runs and testing the rules need no process or disk.

use crate::store::Event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// Started or between stages; nothing has ended it.
    Running,
    /// The run reached its end (the worktree was cleaned up or deliberately kept).
    Completed,
    Failed,
    /// Waiting on a human, for example a rebase conflict.
    Blocked,
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
    let mut summary = RunSummary { phase: RunPhase::Running, stage: None, reason: None };
    for event in events {
        let phase = match event.kind.as_str() {
            "stage.started" | "stage.completed" => RunPhase::Running,
            "stage.failed" | "run.failed" => RunPhase::Failed,
            "run.blocked" => RunPhase::Blocked,
            "worktree.removed" | "worktree.kept" => RunPhase::Completed,
            _ => continue,
        };
        summary.phase = phase;
        summary.reason = match phase {
            RunPhase::Failed | RunPhase::Blocked => {
                event.payload.get("reason").and_then(|r| r.as_str()).map(str::to_string)
            }
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
