//! What a run's events say about a commit it pushed: whether its gate passed, the commands it ran, and CI's result,
//! so a report built after the run ended carries the evidence the run would have given it.

use crate::ci_wait::CiOutcome;
use crate::store::{Event, RunStore, StoreError};

/// The test evidence a run's events hold for one head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedEvidence {
    /// Whether the run's latest gate passed: the gate runs before every push, so that is the pushed head's.
    pub gate_passed: bool,
    /// The commands of that passing gate, when its event names them; gates recorded before they did name none.
    pub gate_steps: Option<Vec<Vec<String>>>,
    /// What CI said about the head; `None` when CI gave no result for it.
    pub ci: Option<CiOutcome>,
}

/// The evidence `run_id`'s events hold for `head_sha`.
pub fn recorded_evidence(store: &RunStore, run_id: &str, head_sha: &str) -> Result<RecordedEvidence, StoreError> {
    Ok(evidence_in(&store.events(run_id)?, head_sha))
}

fn evidence_in(events: &[Event], head_sha: &str) -> RecordedEvidence {
    let ended = |e: &&Event, stage: &str| e.stage.as_deref() == Some(stage) && matches!(e.kind.as_str(), "stage.completed" | "stage.failed" | "stage.skipped");
    let gate = events.iter().rev().find(|e| ended(e, "verify"));
    let gate_passed = gate.is_some_and(|e| e.kind == "stage.completed");
    let gate_steps = gate.filter(|_| gate_passed).and_then(|e| serde_json::from_value(e.payload.get("commands")?.clone()).ok());
    let ci = events
        .iter()
        .rev()
        .filter(|e| ended(e, "ci"))
        // A skip because CI is disabled names no head: it holds for any.
        .filter(|e| e.payload.get("head_sha").and_then(|h| h.as_str()).map_or(e.kind == "stage.skipped", |h| h == head_sha))
        .find_map(ci_outcome);
    RecordedEvidence { gate_passed, gate_steps, ci }
}

fn ci_outcome(event: &Event) -> Option<CiOutcome> {
    let text = |key: &str| event.payload.get(key).and_then(|v| v.as_str()).map(str::to_string);
    match event.kind.as_str() {
        "stage.completed" => Some(CiOutcome::Passed),
        "stage.skipped" => Some(CiOutcome::Skipped),
        _ => match text("result").as_deref() {
            Some("failed") => Some(CiOutcome::Failed { failing: serde_json::from_value(event.payload.get("failing")?.clone()).unwrap_or_default() }),
            Some("timed_out") => Some(CiOutcome::TimedOut),
            Some("errored") => Some(CiOutcome::Errored { reason: text("reason").unwrap_or_default() }),
            _ => None,
        },
    }
}
