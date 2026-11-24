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
    /// For a run blocked on a rebase conflict that a person since finished: the commit that
    /// rebase replayed the branch onto, which the branch now contains.
    pub finished_by_hand: Option<String>,
    /// The run blocked on a rebase conflict that is still in progress, which the resume finishes.
    pub mid_rebase: bool,
    /// Why the session is started or inherited, as `run.resumed` records it.
    pub session_why: String,
}

impl ResumePlan {
    /// Whether the resumed run starts its own session or inherits the finished one, and why.
    pub fn session(&self) -> (&'static str, String) {
        (
            if self.skip_implement {
                "inherited"
            } else {
                "started"
            },
            self.session_why.clone(),
        )
    }
}

/// An answer given with a resume must reach a session: a resume that inherits the finished session
/// refuses it rather than dropping it.
pub fn check_answer_reaches_session(plan: &ResumePlan, answer: Option<&str>) -> Result<(), String> {
    if plan.skip_implement && answer.is_some_and(|a| !a.trim().is_empty()) {
        return Err(
            "this run's session already finished cleanly; an answer cannot reach it".to_string(),
        );
    }
    Ok(())
}

/// Whether a run's ending reason says the review (or the session's own report) stopped it.
fn review_stopped(reason: &str) -> bool {
    review_stop_reason(reason)
        || reason
            .trim()
            .starts_with("the implement session reported a problem")
}

/// Whether the reason is the review's own stop: it starts with the review's wording, so a session
/// problem that merely quotes that wording is not one.
fn review_stop_reason(reason: &str) -> bool {
    let reason = reason.trim();
    reason.starts_with("the review escalated")
        || reason.starts_with("the review could not be finished")
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

/// A stopped run (paused, failed, blocked or waiting on a review) was continued as `new_run_id` on
/// its branch and worktree: it is resumed and then ended, so it no longer holds the checkout it
/// handed over and offers no action of its own.
pub fn supersede_resumed(
    store: &RunStore,
    from: &RunRecord,
    new_run_id: &str,
) -> Result<(), StoreError> {
    supersede_resumed_payload(
        store,
        from,
        new_run_id,
        serde_json::json!({ "resumed_as": new_run_id }),
    )
}

/// [`supersede_resumed`], recording whether the new run `started` its session or `inherited` the
/// finished one, and why.
pub fn supersede_resumed_with_session(
    store: &RunStore,
    from: &RunRecord,
    new_run_id: &str,
    session: &str,
    why: &str,
) -> Result<(), StoreError> {
    supersede_resumed_payload(
        store,
        from,
        new_run_id,
        serde_json::json!({ "resumed_as": new_run_id, "session": session, "why": why }),
    )
}

/// [`supersede_resumed_with_session`], also recording whether the resume went with an answer for
/// the session: `answer: true|false`, never the text.
pub fn supersede_resumed_answered(
    store: &RunStore,
    from: &RunRecord,
    new_run_id: &str,
    session: &str,
    why: &str,
    answered: bool,
) -> Result<(), StoreError> {
    supersede_resumed_payload(
        store,
        from,
        new_run_id,
        serde_json::json!({ "resumed_as": new_run_id, "session": session, "why": why, "answer": answered }),
    )
}

fn supersede_resumed_payload(
    store: &RunStore,
    from: &RunRecord,
    new_run_id: &str,
    resumed: serde_json::Value,
) -> Result<(), StoreError> {
    for (kind, payload) in [
        ("run.resumed", resumed),
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
    plan_group_resume_with(
        store,
        repo,
        project_id,
        members,
        &crate::interrupted::process_is_alive,
    )
}

/// [`plan_group_resume`]; with `fix_conflicts`, only for a run blocked on a rebase conflict that is
/// still in progress, which the resumed run then has a session resolve.
pub fn plan_group_resume_fixing(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
    fix_conflicts: bool,
) -> Result<ResumePlan, String> {
    plan(
        store,
        repo,
        project_id,
        members,
        &crate::interrupted::process_is_alive,
        fix_conflicts,
        false,
        false,
    )
}

/// [`plan_group_resume`], asked to review the run again: a run waiting on a review decision from
/// the review that escalated, and a failed one from its last review, provided something was
/// committed after it. Any other run is refused.
pub fn plan_group_resume_reviewing_again(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
) -> Result<ResumePlan, String> {
    plan(
        store,
        repo,
        project_id,
        members,
        &crate::interrupted::process_is_alive,
        false,
        true,
        false,
    )
}

/// The run of `project_id` named after `feature_id` (a single feature's run, or a group's that it
/// leads) that started last: the one a resume of that feature continues. `None` when it never ran.
/// Nothing is reconciled or changed.
pub fn newest_run_of(
    store: &RunStore,
    project_id: &str,
    feature_id: &str,
) -> Result<Option<String>, String> {
    Ok(newest_record(store, project_id, feature_id)?.map(|record| record.run_id))
}

/// [`newest_run_of`] for each of `features` at once, in one pass over the runs: each feature that ran,
/// to its newest run. Nothing is reconciled or changed.
pub fn newest_runs_of(
    store: &RunStore,
    project_id: &str,
    features: &[&str],
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut latest: std::collections::HashMap<String, RunOrder> = std::collections::HashMap::new();
    let mut runs = std::collections::HashMap::new();
    if features.is_empty() {
        return Ok(runs);
    }
    for run_id in store
        .run_ids()
        .map_err(|e| format!("cannot list the runs: {e:?}"))?
    {
        let Ok(record) = store.load(&run_id) else {
            continue;
        };
        if record.project_id != project_id || !features.contains(&record.feature_id.as_str()) {
            continue;
        }
        let order = run_order(&run_id, &store.events(&run_id).unwrap_or_default());
        if latest
            .get(&record.feature_id)
            .is_none_or(|best| *best < order)
        {
            latest.insert(record.feature_id.clone(), order);
            runs.insert(record.feature_id, run_id);
        }
    }
    Ok(runs)
}

fn newest_record(
    store: &RunStore,
    project_id: &str,
    feature_id: &str,
) -> Result<Option<RunRecord>, String> {
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
        let order = run_order(&run_id, &store.events(&run_id).unwrap_or_default());
        if latest.as_ref().is_none_or(|(best, _)| *best < order) {
            latest = Some((order, record));
        }
    }
    Ok(latest.map(|(_, record)| record))
}

fn run_order(run_id: &str, events: &[Event]) -> RunOrder {
    RunOrder {
        started: events
            .first()
            .map(|e| e.occurred_at.clone())
            .unwrap_or_default(),
        id: id_key(run_id),
    }
}

/// The runs of `project_id` with their order and events. A run whose events cannot be read is an
/// error: when it began and what it took are unknown, so nothing may be assumed about it.
fn project_runs(
    store: &RunStore,
    project_id: &str,
) -> Result<Vec<(RunOrder, RunRecord, Vec<Event>)>, String> {
    let mut runs = Vec::new();
    for run_id in store
        .run_ids()
        .map_err(|e| format!("cannot list the runs: {e:?}"))?
    {
        let Ok(record) = store.load(&run_id) else {
            continue;
        };
        if record.project_id != project_id {
            continue;
        }
        let events = store.events(&run_id).map_err(|e| {
            format!("cannot tell what run {run_id} took: its events cannot be read: {e:?}")
        })?;
        runs.push((run_order(&run_id, &events), record, events));
    }
    Ok(runs)
}

/// The `members` (led by the first) that a resume of their newest run may still take: those no run
/// of `project_id` that started after it took, whatever feature led that run. All of them when the
/// lead never ran. Nothing is reconciled or changed.
pub fn resumable_members(
    store: &RunStore,
    project_id: &str,
    members: &[String],
) -> Result<Vec<String>, String> {
    let Some(lead) = members.first() else {
        return Ok(Vec::new());
    };
    let runs = project_runs(store, project_id)?;
    let Some(newest) = runs
        .iter()
        .filter(|(_, record, _)| &record.feature_id == lead)
        .map(|(order, _, _)| order)
        .max()
    else {
        return Ok(members.to_vec());
    };
    let taken: Vec<String> = runs
        .iter()
        .filter(|(order, _, _)| order > newest)
        .flat_map(|(_, record, events)| members_of(record, events))
        .collect();
    Ok(members
        .iter()
        .filter(|m| !taken.contains(m))
        .cloned()
        .collect())
}

/// [`plan_group_resume`] with the owner-liveness check supplied (pid, recorded start time).
pub fn plan_group_resume_with(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
    alive: &dyn Fn(u32, Option<u64>) -> bool,
) -> Result<ResumePlan, String> {
    plan(store, repo, project_id, members, alive, false, false, false)
}

fn plan(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    members: &[String],
    alive: &dyn Fn(u32, Option<u64>) -> bool,
    fix_conflicts: bool,
    review_again: bool,
    design_issues: bool,
) -> Result<ResumePlan, String> {
    let feature_id = members
        .first()
        .map(String::as_str)
        .ok_or("a run needs a feature")?;
    let Some(from) = newest_record(store, project_id, feature_id)? else {
        return Err(format!("{feature_id} has no earlier run to resume"));
    };
    // A run killed mid-flight never recorded an ending: if its process is gone, it is a failed run.
    crate::interrupted::reconcile_interrupted(store, &from.run_id, alive)
        .map_err(|e| format!("cannot reconcile {}: {e:?}", from.run_id))?;
    let events = store
        .events(&from.run_id)
        .map_err(|e| format!("cannot read the events of {}: {e:?}", from.run_id))?;
    let phase = summarize_run(&events).phase;
    if review_again && !matches!(phase, RunPhase::NeedsReview | RunPhase::Failed) {
        let phase = phase.as_str().replace('_', " ");
        return Err(format!(
            "{} is {phase}: only a run that needs review or failed can be reviewed again",
            from.run_id
        ));
    }
    // A failed run never keeps its features, whether it just now proved dead or an earlier attempt
    // recorded that and died before releasing. Each release checks ownership, so this is repeatable.
    if summarize_run(&events).phase == RunPhase::Failed {
        for feature in members_of(&from, &events) {
            match store.finish_run(&from.project_id, &feature, &from.run_id) {
                Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
                Err(e) => {
                    return Err(format!(
                        "cannot release {feature} of {}: {e:?}",
                        from.run_id
                    ))
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
    if matches!(phase, RunPhase::Failed | RunPhase::NeedsReview)
        && crate::work_is_on_target(repo, &from)
    {
        phase = RunPhase::Merged;
    }
    match phase {
        // Waiting for design files: resumed once they are there, its package checked again first.
        RunPhase::Failed | RunPhase::Blocked | RunPhase::NeedsReview | RunPhase::DesignFilesNeeded => {}
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
        RunPhase::Accepting => {
            return Err(format!(
                "{}: an accept is running; wait for its tests to finish",
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
    // run's branch: the agent and the setup commands run there. A deleted one is said to be gone.
    if !from.worktree.exists() {
        return Err(format!(
            "{}: the worktree {} no longer exists; start a new run instead",
            from.run_id,
            from.worktree.display()
        ));
    }
    if !within_worktrees_dir(repo, &from.worktree) {
        return Err(format!(
            "{}: {} is not under the repository's worktrees directory; start a new run instead",
            from.run_id,
            from.worktree.display()
        ));
    }
    let conflict_onto = match phase {
        RunPhase::Blocked => crate::checkpoint::blocked_on_conflict(&events),
        _ => None,
    };
    let mid_rebase =
        conflict_onto.is_some() && crate::checkpoint::rebase_in_progress(&from.worktree)?;
    if fix_conflicts && !mid_rebase {
        return Err(format!(
            "{}: --fix-conflicts resumes only a run blocked on a rebase conflict that is still in progress; \
             resume it without the flag",
            from.run_id
        ));
    }
    if mid_rebase && !fix_conflicts {
        return Err(crate::checkpoint::mid_rebase_refusal(
            &from.run_id,
            &from.worktree,
        ));
    }
    // A stopped rebase has HEAD detached; the branch it is rebasing has to be the run's.
    let checkout = if let Some(onto) = conflict_onto.as_deref().filter(|_| mid_rebase) {
        crate::checkpoint::rebase_problem(repo, &from.worktree, &from.branch, onto)
    } else {
        checkout_problem(repo, &from.worktree, &from.branch)
            .map_err(|e| format!("cannot inspect the worktree: {e:?}"))
    };
    if let Some(problem) = checkout? {
        return Err(format!(
            "{}: {problem}; finish or abort a stopped rebase, or start a new run",
            from.run_id
        ));
    }
    // A run waiting on a review decision continues only as another review of the one that
    // escalated; without it, nothing says what to review, so it is not resumed as new work.
    // A design run given a person's issues goes on with its design review step instead.
    let design_round = design_issues || crate::design_loop::design_grant_waiting(&events);
    let review_again = match summarize_run(&events).phase {
        RunPhase::NeedsReview if design_round => None,
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
        // A failed run is reviewed again only when asked, from its last review, and only when
        // something came after it: otherwise there is nothing new to review.
        RunPhase::Failed if review_again => {
            let review = crate::review_escalation::last_review(&from.worktree, &from.base_sha)
                .map_err(|e| format!("{}: cannot read the review ledger: {e}", from.run_id))?
                .ok_or_else(|| format!("{}: its worktree has no review to review again; resume it instead", from.run_id))?;
            if review.unreviewed_commits.is_empty() {
                return Err(format!("{}: nothing was committed after its last review; resume it instead", from.run_id));
            }
            Some(review)
        }
        _ => None,
    };
    // A rebase that stopped on a conflict counts as finished by hand only once the branch has what
    // it was replayed onto.
    let finished_by_hand = match conflict_onto {
        Some(_) if mid_rebase => None,
        Some(onto) => {
            if !crate::checkpoint::head_contains(&from.worktree, &onto)? {
                return Err(format!(
                    "{}: the rebase onto {} was not finished: the branch {} does not contain {onto}; rebase it in {} \
                     and finish with `git rebase --continue`, or start a new run",
                    from.run_id,
                    from.target_branch,
                    from.branch,
                    from.worktree.display()
                ));
            }
            Some(onto)
        }
        _ => None,
    };
    // A run its review stopped did not finish its session's work: the session runs again, and the
    // review it asks for covers what comes after the head that escalated.
    let summary = summarize_run(&events);
    let stopped_by_review = matches!(summary.phase, RunPhase::Blocked | RunPhase::Failed)
        && summary.reason.as_deref().is_some_and(review_stopped);
    let mut review_again = review_again;
    if stopped_by_review && review_again.is_none() {
        let found = crate::review_escalation::escalated_review(&from.worktree, &from.base_sha);
        // A run the review itself stopped needs its ledger; any other session problem resumes without it.
        review_again = if summary.reason.as_deref().is_some_and(review_stop_reason) {
            found.map_err(|e| format!("{}: cannot read the review ledger: {e}", from.run_id))?
        } else {
            found.ok().flatten()
        };
    }
    let skip_implement = implement_finished(&events) && !stopped_by_review;
    let session_why = if stopped_by_review {
        format!(
            "the run ended because of its review: {}",
            summary.reason.as_deref().unwrap_or_default()
        )
    } else if skip_implement {
        "the session already finished cleanly".to_string()
    } else {
        "the session had not finished".to_string()
    };
    Ok(ResumePlan {
        from,
        skip_implement,
        checkpoint,
        review_again,
        finished_by_hand,
        mid_rebase,
        session_why,
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
pub fn carry_rebase(
    store: &RunStore,
    record: &RunRecord,
    from_run_id: &str,
) -> Result<(), StoreError> {
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

/// The run that took `run_id` over, with the features of `run_id` it took: the newest run of
/// `run_id`'s feature when that is a later one, as a resume continues it on the same branch and
/// worktree. `None` when `run_id` is the newest run of its feature.
pub fn later_run_of(
    store: &RunStore,
    run_id: &str,
) -> Result<Option<(String, Vec<String>)>, String> {
    let record = store
        .load(run_id)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    let Some(later) = newest_run_of(store, &record.project_id, &record.feature_id)?
        .filter(|newest| newest != run_id)
    else {
        return Ok(None);
    };
    let theirs = run_members(store, &later)?;
    let taken = run_members(store, run_id)?
        .into_iter()
        .filter(|m| theirs.contains(m))
        .collect();
    Ok(Some((later, taken)))
}

/// Whether `other` works on `run`'s branch or in its worktree, as a run that continues it does.
fn shares_checkout(run: &RunRecord, other: &RunRecord) -> bool {
    (!run.branch.is_empty() && other.branch == run.branch)
        || (!run.worktree.as_os_str().is_empty() && other.worktree == run.worktree)
}

/// The newest run of `run_id`'s project that started after it on its branch or in its worktree, as
/// a resume continues a run: that run works in the checkout `run_id` recorded, so what is done to
/// the checkout is its to do. A run of the same features on a checkout of its own is no such run.
pub fn taken_over_by(store: &RunStore, run_id: &str) -> Result<Option<String>, String> {
    let record = store
        .load(run_id)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    let own = run_order(
        run_id,
        &store
            .events(run_id)
            .map_err(|err| format!("cannot read the run: {err:?}"))?,
    );
    // One scan: a run whose record cannot be read may be the one that took this checkout over, and
    // so may a run on it whose events cannot be read; either fails rather than let another run's
    // work be discarded or accepted. Runs elsewhere are not read further.
    let mut newest: Option<(RunOrder, String)> = None;
    for other in store
        .run_ids()
        .map_err(|e| format!("cannot list the runs: {e:?}"))?
    {
        let candidate = match store.load(&other) {
            Ok(candidate) => candidate,
            Err(StoreError::NotFound(_)) => continue,
            Err(e) => return Err(format!(
                "cannot tell whether {other} took over {run_id}: its record cannot be read: {e:?}"
            )),
        };
        if other == run_id
            || candidate.project_id != record.project_id
            || !shares_checkout(&record, &candidate)
        {
            continue;
        }
        let events = store.events(&other).map_err(|e| {
            format!(
                "cannot tell whether {other} took over {run_id}: its events cannot be read: {e:?}"
            )
        })?;
        let order = run_order(&other, &events);
        if order > own && newest.as_ref().is_none_or(|(best, _)| order > *best) {
            newest = Some((order, other));
        }
    }
    Ok(newest.map(|(_, other)| other))
}

/// Whether a run in `phase` holds one of its project's active-run slots: it is running, pausing or
/// paused (a paused run keeps its worktree mid-stage and resumes into the same session). A blocked,
/// failed, review-waiting or ended run waits on a person, not on the machine, and holds none.
pub fn holds_run_slot(phase: RunPhase) -> bool {
    matches!(
        phase,
        RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused
    )
}

/// Whether resuming `run_id` is a start as far as its project's run limit goes: a run [`holds_run_slot`]
/// says holds no slot (blocked, failed, waiting on review) needs one; a paused or pausing run's resume
/// keeps the slot it already holds. A run whose events still say running but whose process is gone
/// is counted among the active runs until the resume records it as failed, so either way the resume
/// adds no run beyond the limit. Whether the run can be resumed at all is the resume planner's call.
pub fn resume_takes_a_slot(store: &RunStore, run_id: &str) -> Result<bool, String> {
    let events = store
        .events(run_id)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    if events.is_empty() {
        store
            .load(run_id)
            .map_err(|err| format!("cannot read the run: {err:?}"))?;
    }
    Ok(!holds_run_slot(
        crate::run_status::summarize_live_run(&events).phase,
    ))
}

/// The runs among `runs` (run id, record, events) that are active, as (run id, feature id) in the
/// order given: those [`holds_run_slot`] says hold a slot, less any a later run of the same project
/// continued on its branch or in its worktree, which is that later run's business whatever the
/// run's own events last said.
pub fn active_runs(runs: &[(String, RunRecord, Vec<Event>)]) -> Vec<(String, String)> {
    use std::collections::HashMap;
    let ordered: Vec<RunOrder> = runs
        .iter()
        .map(|(run_id, _, events)| run_order(run_id, events))
        .collect();
    // The latest run of each project on each branch and in each worktree, so whether a later run
    // continued a run is one lookup per run rather than a scan of every other run.
    let mut by_branch: HashMap<(&str, &str), &RunOrder> = HashMap::new();
    let mut by_worktree: HashMap<(&str, &Path), &RunOrder> = HashMap::new();
    for ((_, record, _), order) in runs.iter().zip(&ordered) {
        if !record.branch.is_empty() {
            let latest = by_branch
                .entry((&record.project_id, &record.branch))
                .or_insert(order);
            *latest = (*latest).max(order);
        }
        if !record.worktree.as_os_str().is_empty() {
            let latest = by_worktree
                .entry((&record.project_id, &record.worktree))
                .or_insert(order);
            *latest = (*latest).max(order);
        }
    }
    let continued = |record: &RunRecord, own: &RunOrder| {
        let later = |latest: Option<&&RunOrder>| latest.is_some_and(|latest| *latest > own);
        (!record.branch.is_empty()
            && later(by_branch.get(&(record.project_id.as_str(), record.branch.as_str()))))
            || (!record.worktree.as_os_str().is_empty()
                && later(by_worktree.get(&(record.project_id.as_str(), record.worktree.as_path()))))
    };
    runs.iter()
        .zip(&ordered)
        // A run whose stage failed while its process goes on to fix it still holds its slot.
        .filter(|((_, _, events), _)| {
            holds_run_slot(crate::run_status::summarize_live_run(events).phase)
        })
        .filter(|((_, record, _), own)| !continued(record, own))
        .map(|((run_id, record, _), _)| (run_id.clone(), record.feature_id.clone()))
        .collect()
}

/// The pipeline continues the latest run of a run's features: `Ok` when the group resume is valid
/// and starts from `run_id`, the reason otherwise. Nothing is launched.
pub fn check_group_resume(store: &RunStore, repo: &Path, run_id: &str) -> Result<(), String> {
    check_group_resume_fixing(store, repo, run_id, false)
}

/// [`check_group_resume`] for a resume that, with `fix_conflicts`, has a session resolve the rebase
/// the run blocked on.
pub fn check_group_resume_fixing(
    store: &RunStore,
    repo: &Path,
    run_id: &str,
    fix_conflicts: bool,
) -> Result<(), String> {
    check_group_resume_as(store, run_id, &|record, members| {
        plan_group_resume_fixing(store, repo, &record.project_id, members, fix_conflicts)
    })
}

/// [`check_group_resume`] for a resume that gives the person's issues to a design run, whose design
/// review step then goes on with an extra correction round (checked by `check_design_person_issues`).
pub fn check_group_resume_with_design_issues(
    store: &RunStore,
    repo: &Path,
    run_id: &str,
) -> Result<(), String> {
    check_group_resume_as(store, run_id, &|record, members| {
        plan(
            store,
            repo,
            &record.project_id,
            members,
            &crate::interrupted::process_is_alive,
            false,
            false,
            true,
        )
    })
}

/// [`check_group_resume_fixing`] for a resume asked to review the run again
/// ([`plan_group_resume_reviewing_again`]).
pub fn check_group_resume_reviewing_again(
    store: &RunStore,
    repo: &Path,
    run_id: &str,
) -> Result<(), String> {
    check_group_resume_as(store, run_id, &|record, members| {
        plan_group_resume_reviewing_again(store, repo, &record.project_id, members)
    })
}

fn check_group_resume_as(
    store: &RunStore,
    run_id: &str,
    plan: &dyn Fn(&RunRecord, &[String]) -> Result<ResumePlan, String>,
) -> Result<(), String> {
    let record = store
        .load(run_id)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    let members = run_members(store, run_id)?;
    // A later run named after the same feature took it over, whatever else it was for.
    if let Some((later, taken)) = later_run_of(store, run_id)? {
        return Err(if run_members(store, &later)? == members {
            format!("{later} is a later run of the same features; resume that one")
        } else {
            format!(
                "{later} is a later run that took {}; this run can no longer be resumed",
                taken.join(", ")
            )
        });
    }
    plan(&record, &members).map(|_| ())
}

/// Where run `run_id` would continue if resumed now: the next stage of a paused run's checkpoint,
/// "where it stopped" for a failed, blocked, needs-review or design-files-needed run, or the reason it cannot be resumed.
/// Nothing is launched; a run whose process is gone is first recorded as failed, as it already is.
pub fn resume_check(store: &RunStore, repo: &Path, run_id: &str) -> Result<String, String> {
    crate::interrupted::reconcile_interrupted(store, run_id, &crate::interrupted::process_is_alive)
        .map_err(|err| format!("cannot read the run: {err:?}"))?;
    let stopped = store
        .events(run_id)
        .map(|events| {
            matches!(
                summarize_run(&events).phase,
                RunPhase::Failed
                    | RunPhase::Blocked
                    | RunPhase::NeedsReview
                    | RunPhase::DesignFilesNeeded
            )
        })
        .unwrap_or(false);
    if stopped {
        // The pipeline continues the latest run of these features, so that is what is checked.
        check_group_resume(store, repo, run_id)?;
        return Ok("where it stopped".to_string());
    }
    let checkpoint = crate::checkpoint::validate_checkpoint(store, repo, run_id)?;
    Ok(checkpoint
        .next_stage
        .unwrap_or_else(|| "the end".to_string()))
}
