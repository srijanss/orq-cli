//! Picking up a run that stopped. A run that failed or is blocked leaves its branch and worktree
//! behind; resuming reuses them in a new run. Every stage after the agent session can be repeated
//! safely (setup, rebase, gate, push, drafting, and opening the review, which adopts an existing
//! one), so only the agent session is skipped, and only when it already finished.

use std::path::Path;

use crate::implement::{next_event_id, now_rfc3339};
use crate::reap::{checkout_problem, within_worktrees_dir};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const IMPLEMENT_STAGE: &str = "implement_and_review";
const REBASE_CARRIED: &str = "run.rebase_carried";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// The stopped run, whose branch, worktree and base commit the new run takes over.
    pub from: RunRecord,
    /// The agent session ended well in that run and left a commit, so it is not started again.
    pub skip_implement: bool,
    /// For a paused run: where it stopped and what it had already done (and pushed or opened).
    pub checkpoint: Option<crate::checkpoint::Checkpoint>,
    /// For a run waiting on a review decision: the review that escalated, to review again from.
    pub review_again: Option<crate::review_escalation::EscalatedReview>,
}

const GROUP_EVENT: &str = "run.grouped";

/// Note which features, in which order, a run was started for. A run without this note was of
/// its own feature alone.
pub fn record_group(
    store: &RunStore,
    record: &RunRecord,
    members: &[String],
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: GROUP_EVENT.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "features": members }),
    })
}

/// The features a run was for, in order. The first names the run, so a note that does not start
/// with the run's own feature is no group at all.
pub(crate) fn members_of(record: &RunRecord, events: &[Event]) -> Vec<String> {
    events
        .iter()
        .find(|e| e.kind == GROUP_EVENT)
        .and_then(|e| serde_json::from_value::<Vec<String>>(e.payload["features"].clone()).ok())
        .filter(|members| members.first() == Some(&record.feature_id))
        .unwrap_or_else(|| vec![record.feature_id.clone()])
}

/// The features a run was for, in order.
pub fn run_members(store: &RunStore, run_id: &str) -> Result<Vec<String>, String> {
    let record = store
        .load(run_id)
        .map_err(|e| format!("cannot read {run_id}: {e:?}"))?;
    let events = store
        .events(run_id)
        .map_err(|e| format!("cannot read the events of {run_id}: {e:?}"))?;
    Ok(members_of(&record, &events))
}

/// A paused run was continued as `new_run_id`: the paused run is resumed and then ended, so it no
/// longer holds the checkout it handed over.
pub fn supersede_paused(
    store: &RunStore,
    from: &RunRecord,
    new_run_id: &str,
) -> Result<(), StoreError> {
    for (kind, payload) in [
        (
            "run.resumed",
            serde_json::json!({ "resumed_as": new_run_id }),
        ),
        (
            "run.completed",
            serde_json::json!({ "ending": "resumed", "reason": format!("continued as {new_run_id}") }),
        ),
    ] {
        store.append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(&from.run_id),
            run_id: from.run_id.clone(),
            project_id: from.project_id.clone(),
            feature_id: from.feature_id.clone(),
            kind: kind.to_string(),
            stage: None,
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload,
        })?;
    }
    Ok(())
}

/// The latest run of the feature, if it stopped in a way that can be picked up.
pub fn plan_resume(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    feature_id: &str,
) -> Result<ResumePlan, String> {
    plan_group_resume(store, repo, project_id, &[feature_id.to_string()])
}

/// As [`plan_resume`] for a run of `members` (in order); the first names the run. The stopped run
/// must have been for exactly these features, in this order.
pub fn plan_group_resume(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
) -> Result<ResumePlan, String> {
    plan_group_resume_with(store, repo, project_id, members, &crate::interrupted::process_is_alive)
}

/// [`plan_group_resume`] with the owner-liveness check supplied (pid, recorded start time).
pub fn plan_group_resume_with(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
    alive: &dyn Fn(u32, Option<u64>) -> bool,
) -> Result<ResumePlan, String> {
    let feature_id = members
        .first()
        .map(String::as_str)
        .ok_or("a run needs a feature")?;
    let mut latest: Option<(RunOrder, RunRecord)> = None;
    for run_id in store
        .run_ids()
        .map_err(|e| format!("cannot list the runs: {e:?}"))?
    {
        let Ok(record) = store.load(&run_id) else {
            continue;
        };
        if record.project_id != project_id || record.feature_id != feature_id {
            continue;
        }
        let started = store
            .events(&run_id)
            .ok()
            .and_then(|events| events.first().map(|e| e.occurred_at.clone()))
            .unwrap_or_default();
        let order = RunOrder {
            started,
            id: id_key(&run_id),
        };
        if latest.as_ref().is_none_or(|(best, _)| *best < order) {
            latest = Some((order, record));
        }
    }
    let Some((_, from)) = latest else {
        return Err(format!("{feature_id} has no earlier run to resume"));
    };
    // A run killed mid-flight never recorded an ending: if its process is gone, it is a failed run.
    crate::interrupted::reconcile_interrupted(store, &from.run_id, alive)
        .map_err(|e| format!("cannot reconcile {}: {e:?}", from.run_id))?;
    let events = store
        .events(&from.run_id)
        .map_err(|e| format!("cannot read the events of {}: {e:?}", from.run_id))?;
    // A failed run never keeps its features, whether it just now proved dead or an earlier attempt
    // recorded that and died before releasing. Each release checks ownership, so this is repeatable.
    if summarize_run(&events).phase == RunPhase::Failed {
        for feature in members_of(&from, &events) {
            match store.finish_run(&from.project_id, &feature, &from.run_id) {
                Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
                Err(e) => {
                    return Err(format!("cannot release {feature} of {}: {e:?}", from.run_id))
                }
            }
        }
    }
    let earlier = members_of(&from, &events);
    if earlier != members {
        return Err(format!(
            "{} was a run of {}, not of {}; resume it with the same features in the same order",
            from.run_id,
            earlier.join(", "),
            members.join(", ")
        ));
    }
    let mut checkpoint = None;
    let mut phase = summarize_run(&events).phase;
    // The events never say a run was merged by hand: only its branch and the target can tell.
    if matches!(phase, RunPhase::Failed | RunPhase::NeedsReview) && crate::work_is_on_target(repo, &from) {
        phase = RunPhase::Merged;
    }
    match phase {
        RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview => {}
        RunPhase::Running => {
            return Err(format!(
                "{} did not stop; it may still be running",
                from.run_id
            ))
        }
        RunPhase::Completed => {
            return Err(format!(
                "{} ended with a review ready; there is nothing to resume",
                from.run_id
            ))
        }
        RunPhase::Merged => {
            return Err(format!(
                "{}'s work is already on its target branch; there is nothing to resume",
                from.run_id
            ))
        }
        RunPhase::Paused => {
            // A paused run continues only from a checkpoint that still describes its checkout.
            checkpoint = Some(crate::checkpoint::validate_checkpoint(
                store,
                repo,
                &from.run_id,
            )?);
        }
        phase @ RunPhase::PauseRequested => {
            return Err(format!(
                "{} is {}; it has not paused yet",
                from.run_id,
                phase.as_str()
            ))
        }
    }
    // The saved path is only trusted after it proves to be this repository's checkout of the
    // run's branch: the agent and the setup commands run there.
    if !within_worktrees_dir(repo, &from.worktree) {
        return Err(format!(
            "{}: {} is not under the repository's worktrees directory; start a new run instead",
            from.run_id,
            from.worktree.display()
        ));
    }
    if let Some(problem) = checkout_problem(repo, &from.worktree, &from.branch)
        .map_err(|e| format!("cannot inspect the worktree: {e:?}"))?
    {
        return Err(format!(
            "{}: {problem}; finish or abort a stopped rebase, or start a new run",
            from.run_id
        ));
    }
    // A run waiting on a review decision continues only as another review of the one that
    // escalated; without it, nothing says what to review, so it is not resumed as new work.
    let review_again = match summarize_run(&events).phase {
        RunPhase::NeedsReview => Some(
            crate::review_escalation::escalated_review(&from.worktree, &from.base_sha)
                .map_err(|e| format!("{}: cannot read the review ledger: {e}", from.run_id))?
                .ok_or_else(|| {
                    format!(
                        "{}: its worktree has no escalated review to review again; accept or discard it",
                        from.run_id
                    )
                })?,
        ),
        _ => None,
    };
    let skip_implement = implement_finished(&events);
    Ok(ResumePlan {
        from,
        skip_implement,
        checkpoint,
        review_again,
    })
}

/// When a run began (its first event; RFC 3339 UTC text sorts by time), then its id with number
/// runs compared as numbers, since ids are `run-<seconds>-<pid>` and pids vary in width.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RunOrder {
    started: String,
    id: Vec<(u64, String)>,
}

fn id_key(run_id: &str) -> Vec<(u64, String)> {
    run_id
        .split('-')
        .map(|part| (part.parse::<u64>().unwrap_or(0), part.to_string()))
        .collect()
}

/// The agent session completed and nothing afterwards blocked on its result.
fn implement_finished(events: &[Event]) -> bool {
    let mut finished = false;
    for event in events
        .iter()
        .filter(|e| e.stage.as_deref() == Some(IMPLEMENT_STAGE))
    {
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
pub fn record_inherited_implement(
    store: &RunStore,
    record: &RunRecord,
    from_run_id: &str,
) -> Result<(), StoreError> {
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

/// The commit `run_id`'s branch was last rebased onto, by this run or by a run it resumed.
pub fn rebased_onto(store: &RunStore, run_id: &str) -> Result<Option<String>, StoreError> {
    Ok(store
        .events(run_id)?
        .iter()
        .rev()
        .filter(|e| e.kind == "rebase.completed" || e.kind == REBASE_CARRIED)
        .find_map(|e| e.payload["target_sha"].as_str().map(str::to_string)))
}

/// Tell `record`'s run what the branch it takes over from `from_run_id` sits on: a resumed run can
/// skip its rebase. Only appends to the new run.
pub fn carry_rebase(store: &RunStore, record: &RunRecord, from_run_id: &str) -> Result<(), StoreError> {
    let Some(target_sha) = rebased_onto(store, from_run_id)? else {
        return Ok(());
    };
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: REBASE_CARRIED.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "from": from_run_id, "target_sha": target_sha }),
    })
}

/// The pipeline continues the latest run of a run's features: `Ok` when the group resume is valid
/// and starts from `run_id`, the reason otherwise. Nothing is launched.
pub fn check_group_resume(store: &RunStore, repo: &Path, run_id: &str) -> Result<(), String> {
    let record = store.load(run_id).map_err(|err| format!("cannot read the run: {err:?}"))?;
    let members = run_members(store, run_id)?;
    let plan = plan_group_resume(store, repo, &record.project_id, &members)?;
    if plan.from.run_id == run_id {
        Ok(())
    } else {
        Err(format!("{} is a later run of the same features; resume that one", plan.from.run_id))
    }
}

/// Where run `run_id` would continue if resumed now: the next stage of a paused run's checkpoint,
/// "where it stopped" for a failed, blocked or needs-review run, or the reason it cannot be resumed.
/// Nothing is launched; a run whose process is gone is first recorded as failed, as it already is.
pub fn resume_check(store: &RunStore, repo: &Path, run_id: &str) -> Result<String, String> {
    crate::interrupted::reconcile_interrupted(store, run_id, &crate::interrupted::process_is_alive)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    let stopped = store
        .events(run_id)
        .map(|events| matches!(summarize_run(&events).phase, RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview))
        .unwrap_or(false);
    if stopped {
        // The pipeline continues the latest run of these features, so that is what is checked.
        check_group_resume(store, repo, run_id)?;
        return Ok("where it stopped".to_string());
    }
    let checkpoint = crate::checkpoint::validate_checkpoint(store, repo, run_id)?;
    Ok(checkpoint.next_stage.unwrap_or_else(|| "the end".to_string()))
}
