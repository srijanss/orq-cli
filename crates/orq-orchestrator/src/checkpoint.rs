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
pub fn record_checkpoint(
    store: &RunStore,
    run_id: &str,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
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
    Ok(events
        .iter()
        .rev()
        .find(|e| e.kind == KIND)
        .and_then(|e| serde_json::from_value(e.payload.clone()).ok()))
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
    if crate::run_status::summarize_run(&events).phase
        != crate::run_status::RunPhase::PauseRequested
    {
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
        return Err(StoreError::Corrupt(format!(
            "cannot read HEAD of {}",
            record.worktree.display()
        )));
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

/// Whether a paused run can be resumed from its latest checkpoint: the run is paused, the worktree
/// is the run's own checkout of its branch, HEAD is where the pause left it and no rebase is under
/// way. The error is the reason; nothing is changed either way.
pub fn validate_checkpoint(
    store: &RunStore,
    repo: &std::path::Path,
    run_id: &str,
) -> Result<Checkpoint, String> {
    let events = store
        .events(run_id)
        .map_err(|e| format!("cannot read the run: {e:?}"))?;
    let phase = crate::run_status::summarize_run(&events).phase;
    if phase != crate::run_status::RunPhase::Paused {
        return Err(format!(
            "{run_id} is {}; only a paused run can be resumed",
            phase.as_str()
        ));
    }
    let checkpoint = latest_checkpoint(store, run_id)
        .map_err(|e| format!("cannot read the checkpoint: {e:?}"))?
        .ok_or_else(|| format!("{run_id} has no checkpoint"))?;
    let record = store
        .load(run_id)
        .map_err(|e| format!("cannot read the run: {e:?}"))?;
    if checkpoint.worktree != record.worktree || checkpoint.branch != record.branch {
        return Err("the checkpoint does not match the run's branch and worktree".to_string());
    }
    if !checkpoint.worktree.is_dir() {
        return Err(format!(
            "the worktree {} no longer exists",
            checkpoint.worktree.display()
        ));
    }
    if !crate::reap::within_worktrees_dir(repo, &checkpoint.worktree) {
        return Err(format!(
            "{} is not under the repository's worktrees directory",
            checkpoint.worktree.display()
        ));
    }
    if let Some(problem) =
        crate::reap::checkout_problem(repo, &checkpoint.worktree, &checkpoint.branch)
            .map_err(|e| format!("cannot inspect the worktree: {e:?}"))?
    {
        return Err(problem);
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&checkpoint.worktree)
            .args(args)
            .output()
    };
    let head = git(&["rev-parse", "HEAD"]).map_err(|e| format!("cannot run git: {e}"))?;
    if String::from_utf8_lossy(&head.stdout).trim() != checkpoint.head_sha {
        return Err("HEAD moved since the run was paused".to_string());
    }
    if rebase_in_progress(&checkpoint.worktree)? {
        return Err("a rebase is in progress in the worktree".to_string());
    }
    Ok(checkpoint)
}

/// Whether a rebase stopped part way in `worktree`.
pub fn rebase_in_progress(worktree: &std::path::Path) -> Result<bool, String> {
    for marker in ["rebase-merge", "rebase-apply"] {
        let path = std::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["rev-parse", "--path-format=absolute", "--git-path", marker])
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if std::path::Path::new(String::from_utf8_lossy(&path.stdout).trim()).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Why a run blocked on a rebase conflict cannot resume while that rebase is still in progress,
/// with the two ways on.
pub fn mid_rebase_refusal(run_id: &str, worktree: &std::path::Path) -> String {
    format!(
        "{run_id}: its rebase stopped on a conflict and is still in progress in {}; resolve the conflict there and \
         finish with `git rebase --continue`, then resume it again, or let a session resolve it with \
         `orq run resume {run_id} --fix-conflicts`",
        worktree.display()
    )
}

/// The commit a run's rebase was replaying onto when it stopped on a conflict, if the run then
/// blocked on it: its last rebase event is `rebase.conflict`, followed by `run.blocked`.
pub fn blocked_on_conflict(events: &[Event]) -> Option<String> {
    pending_conflict(events)?.payload["onto"].as_str().map(str::to_string)
}

/// The `rebase.conflict` event a run blocked on, if it did: its last rebase event, followed by
/// `run.blocked`.
fn pending_conflict(events: &[Event]) -> Option<&Event> {
    let last = events.iter().rposition(|e| e.kind.starts_with("rebase."))?;
    if events[last].kind != "rebase.conflict" || !events[last + 1..].iter().any(|e| e.kind == "run.blocked") {
        return None;
    }
    Some(&events[last])
}

/// A run resumed to finish the rebase `from_run_id` blocked on, still in progress, takes over that
/// conflict: should it block on it too, it is resumed the same way.
pub fn carry_conflict(store: &RunStore, record: &crate::store::RunRecord, from_run_id: &str) -> Result<(), StoreError> {
    let events = store.events(from_run_id)?;
    let Some(conflict) = pending_conflict(&events) else {
        return Ok(());
    };
    let mut payload = conflict.payload.clone();
    payload["carried_from"] = serde_json::json!(from_run_id);
    crate::rebase::record_event(store, record, "rebase.conflict", payload)
}

/// Whether `worktree`'s HEAD contains `onto`, a full commit id: anything else, an option or a
/// name, never reaches git and counts as not contained.
pub fn head_contains(worktree: &std::path::Path, onto: &str) -> Result<bool, String> {
    let full_id = matches!(onto.len(), 40 | 64) && onto.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !full_id {
        return Ok(false);
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["merge-base", "--is-ancestor", onto, "HEAD"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    Ok(out.status.success())
}

/// A person finished the rebase a run blocked on, onto `onto`: recorded on `record`'s run, which
/// resumes it, as that run's branch now sits on it.
pub fn record_rebase_finished_by_hand(store: &RunStore, record: &crate::store::RunRecord, onto: &str) -> Result<(), StoreError> {
    crate::rebase::record_event(store, record, "rebase.completed", serde_json::json!({ "resolved_by": "person", "target_sha": onto }))
}

/// Why a run's worktree, stopped part way through a rebase, cannot have that rebase finished for
/// the run: it is not one of `repo`'s worktrees, or the rebase is not the one the run blocked on,
/// of the run's `branch` onto `onto`.
pub fn rebase_problem(repo: &std::path::Path, worktree: &std::path::Path, branch: &str, onto: &str) -> Result<Option<String>, String> {
    if let Some(problem) = crate::reap::repository_problem(repo, worktree).map_err(|e| format!("cannot inspect the worktree: {e:?}"))? {
        return Ok(Some(problem));
    }
    for dir in ["rebase-merge", "rebase-apply"] {
        let path = std::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["rev-parse", "--path-format=absolute", "--git-path", dir])
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        let dir = std::path::PathBuf::from(String::from_utf8_lossy(&path.stdout).trim());
        let Ok(name) = std::fs::read_to_string(dir.join("head-name")) else {
            continue;
        };
        if name.trim() != format!("refs/heads/{branch}") {
            return Ok(Some("the rebase in the worktree is not of the branch of the run".to_string()));
        }
        let replaying_onto = std::fs::read_to_string(dir.join("onto")).unwrap_or_default();
        if replaying_onto.trim() != onto {
            return Ok(Some(format!(
                "the rebase in the worktree is not the one the run blocked on, onto {onto}; abort it and finish that one"
            )));
        }
        return Ok(None);
    }
    Ok(Some("the worktree is not on the branch of the run".to_string()))
}
