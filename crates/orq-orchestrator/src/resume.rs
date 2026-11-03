//! Picking up a run that stopped. A run that failed or is blocked leaves its branch and worktree
//! behind; resuming reuses them in a new run. Every stage after the agent session can be repeated
//! safely (setup, rebase, gate, push, drafting, and opening the review, which adopts an existing
//! one), so only the agent session is skipped, and only when it already finished.

use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunRecord, RunStore};

const IMPLEMENT_STAGE: &str = "implement_and_review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// The stopped run, whose branch, worktree and base commit the new run takes over.
    pub from: RunRecord,
    /// The agent session ended well in that run and left a commit, so it is not started again.
    pub skip_implement: bool,
}

/// The latest run of the feature, if it stopped in a way that can be picked up.
pub fn plan_resume(store: &RunStore, project_id: &str, feature_id: &str) -> Result<ResumePlan, String> {
    let mut latest: Option<RunRecord> = None;
    for run_id in store.run_ids().map_err(|e| format!("cannot list the runs: {e:?}"))? {
        let Ok(record) = store.load(&run_id) else { continue };
        if record.project_id == project_id && record.feature_id == feature_id && latest.as_ref().is_none_or(|l| l.run_id < record.run_id) {
            latest = Some(record);
        }
    }
    let Some(from) = latest else { return Err(format!("{feature_id} has no earlier run to resume")) };
    let events = store.events(&from.run_id).map_err(|e| format!("cannot read the events of {}: {e:?}", from.run_id))?;
    match summarize_run(&events).phase {
        RunPhase::Failed | RunPhase::Blocked => {}
        RunPhase::Running => return Err(format!("{} did not stop; it may still be running", from.run_id)),
        RunPhase::Completed => return Err(format!("{} ended with a review ready; there is nothing to resume", from.run_id)),
    }
    // A linked worktree has a `.git` file; the repository itself (a `--publish` run) has a directory.
    if !from.worktree.join(".git").is_file() {
        return Err(format!("{} has no worktree of its own left at {}; start a new run instead", from.run_id, from.worktree.display()));
    }
    let skip_implement = implement_finished(&events);
    Ok(ResumePlan { from, skip_implement })
}

/// The agent session completed and nothing afterwards blocked on its result.
fn implement_finished(events: &[Event]) -> bool {
    let mut finished = false;
    for event in events.iter().filter(|e| e.stage.as_deref() == Some(IMPLEMENT_STAGE)) {
        match event.kind.as_str() {
            "stage.completed" => finished = true,
            "stage.started" | "stage.failed" | "run.blocked" => finished = false,
            _ => {}
        }
    }
    finished
}
