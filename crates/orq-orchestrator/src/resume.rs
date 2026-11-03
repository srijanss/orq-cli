//! Picking up a run that stopped. A run that failed or is blocked leaves its branch and worktree
//! behind; resuming reuses them in a new run. Every stage after the agent session can be repeated
//! safely (setup, rebase, gate, push, drafting, and opening the review, which adopts an existing
//! one), so only the agent session is skipped, and only when it already finished.

use std::path::Path;

use crate::reap::{checkout_problem, within_worktrees_dir};
use crate::run_status::{summarize_run, RunPhase};
use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const IMPLEMENT_STAGE: &str = "implement_and_review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// The stopped run, whose branch, worktree and base commit the new run takes over.
    pub from: RunRecord,
    /// The agent session ended well in that run and left a commit, so it is not started again.
    pub skip_implement: bool,
}

/// The latest run of the feature, if it stopped in a way that can be picked up.
pub fn plan_resume(store: &RunStore, repo: &Path, project_id: &str, feature_id: &str) -> Result<ResumePlan, String> {
    let mut latest: Option<(RunOrder, RunRecord)> = None;
    for run_id in store.run_ids().map_err(|e| format!("cannot list the runs: {e:?}"))? {
        let Ok(record) = store.load(&run_id) else { continue };
        if record.project_id != project_id || record.feature_id != feature_id {
            continue;
        }
        let started = store.events(&run_id).ok().and_then(|events| events.first().map(|e| e.occurred_at.clone())).unwrap_or_default();
        let order = RunOrder { started, id: id_key(&run_id) };
        if latest.as_ref().is_none_or(|(best, _)| *best < order) {
            latest = Some((order, record));
        }
    }
    let Some((_, from)) = latest else { return Err(format!("{feature_id} has no earlier run to resume")) };
    let events = store.events(&from.run_id).map_err(|e| format!("cannot read the events of {}: {e:?}", from.run_id))?;
    match summarize_run(&events).phase {
        RunPhase::Failed | RunPhase::Blocked => {}
        RunPhase::Running => return Err(format!("{} did not stop; it may still be running", from.run_id)),
        RunPhase::Completed => return Err(format!("{} ended with a review ready; there is nothing to resume", from.run_id)),
    }
    // The saved path is only trusted after it proves to be this repository's checkout of the
    // run's branch: the agent and the setup commands run there.
    if !within_worktrees_dir(repo, &from.worktree) {
        return Err(format!("{}: {} is not under the repository's worktrees directory; start a new run instead", from.run_id, from.worktree.display()));
    }
    if let Some(problem) = checkout_problem(repo, &from.worktree, &from.branch).map_err(|e| format!("cannot inspect the worktree: {e:?}"))? {
        return Err(format!("{}: {problem}; finish or abort a stopped rebase, or start a new run", from.run_id));
    }
    let skip_implement = implement_finished(&events);
    Ok(ResumePlan { from, skip_implement })
}

/// When a run began (its first event; RFC 3339 UTC text sorts by time), then its id with number
/// runs compared as numbers, since ids are `run-<seconds>-<pid>` and pids vary in width.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RunOrder {
    started: String,
    id: Vec<(u64, String)>,
}

fn id_key(run_id: &str) -> Vec<(u64, String)> {
    run_id.split('-').map(|part| (part.parse::<u64>().unwrap_or(0), part.to_string())).collect()
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

/// Carry the finished agent session over to the resumed run, so a run resumed again after
/// stopping later still knows the session is done.
pub fn record_inherited_implement(store: &RunStore, record: &RunRecord, from_run_id: &str) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "stage.completed".to_string(),
        stage: Some(IMPLEMENT_STAGE.to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "resumed_from": from_run_id }),
    })
}
