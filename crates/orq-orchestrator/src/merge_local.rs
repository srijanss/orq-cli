//! A merge-local run delivers its work without a remote: the base branch of the run's own
//! repository is fast-forwarded to the run's branch. Never a merge commit, never a push.

use std::path::PathBuf;

use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "merge_local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The base branch now points at the run's head.
    Merged { head_sha: String },
    /// The base branch was left where it was, and why.
    Refused { reason: String },
    /// The base branch moved past the commit the run was rebased onto, to `base_sha`, and was
    /// left there: rebasing the run onto `base_sha` makes the fast-forward possible again.
    BaseMoved { base_sha: String },
}

/// Fast-forward `run_id`'s target branch to the run's branch, in the checkout that has it.
pub fn merge_into_base(store: &RunStore, run_id: &str) -> Result<MergeOutcome, StoreError> {
    let record = store.load(run_id)?;
    let head = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.branch)])?;
    if !head.success {
        return Err(StoreError::Corrupt(format!("the run branch {} has no commit", record.branch)));
    }
    let head_sha = head.stdout.trim().to_string();
    let target = format!("refs/heads/{}", record.target_branch);
    // Where the local base branch is now: the run was rebased onto the fetched target, which the
    // local branch may be behind, and only this commit is fast-forwarded from.
    let base = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("{target}^{{commit}}")])?;
    if !base.success {
        return Err(StoreError::Corrupt(format!("the base branch {} has no commit", record.target_branch)));
    }
    let base_sha = base.stdout.trim().to_string();
    let fast_forward = git_raw(&record.worktree, &["merge-base", "--is-ancestor", &base_sha, &head_sha])?;
    if !fast_forward.success {
        return Ok(MergeOutcome::BaseMoved { base_sha });
    }
    match checkout_of(&record)? {
        Some(checkout) => {
            let status = git_raw(&checkout, &["status", "--porcelain", "--untracked-files=no"])?;
            if !status.stdout.trim().is_empty() {
                return Ok(MergeOutcome::Refused {
                    reason: format!("{} is checked out at {} with uncommitted changes; commit or stash them, then resume this run", record.target_branch, checkout.display()),
                });
            }
            // A local file in the way, ignored or untracked, makes git stop rather than overwrite it.
            let merged = git_raw(&checkout, &["merge", "--ff-only", "--no-overwrite-ignore", "--quiet", &head_sha])?;
            if !merged.success {
                if let Some(base_sha) = moved_on_from(&record, &base_sha)? {
                    return Ok(MergeOutcome::BaseMoved { base_sha });
                }
                return Ok(MergeOutcome::Refused {
                    reason: format!("{} at {} could not be fast-forwarded: {}", record.target_branch, checkout.display(), merged.stderr.trim()),
                });
            }
        }
        // With no checkout to update, only the ref moves, and only from the commit checked above,
        // so a base branch that moved meanwhile is never overwritten.
        None => {
            let reason = format!("orq: merge-local {}", record.run_id);
            let moved = git_raw(&record.worktree, &["update-ref", "-m", &reason, &target, &head_sha, &base_sha])?;
            if !moved.success {
                if let Some(base_sha) = moved_on_from(&record, &base_sha)? {
                    return Ok(MergeOutcome::BaseMoved { base_sha });
                }
                return Ok(MergeOutcome::Refused { reason: format!("{} could not be moved to this run's branch: {}", record.target_branch, moved.stderr.trim()) });
            }
        }
    }
    record_event(store, &record, "merge.completed", serde_json::json!({ "head_sha": head_sha, "target_branch": record.target_branch }))?;
    Ok(MergeOutcome::Merged { head_sha })
}

/// Where the base branch is now, when a fast-forward from `base_sha` failed because it moved on
/// meanwhile; `None` when it is still there and the failure has another cause.
fn moved_on_from(record: &RunRecord, base_sha: &str) -> Result<Option<String>, StoreError> {
    let now = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.target_branch)])?;
    let now = now.stdout.trim();
    Ok((!now.is_empty() && now != base_sha).then(|| now.to_string()))
}

/// The working tree that has the target branch checked out, if any does.
fn checkout_of(record: &RunRecord) -> Result<Option<PathBuf>, StoreError> {
    // NUL-separated, so a path is read whole whatever characters it holds.
    let listed = git_raw(&record.worktree, &["worktree", "list", "--porcelain", "-z"])?;
    if !listed.success {
        return Err(StoreError::Corrupt(format!("cannot list the repository's worktrees: {}", listed.stderr.trim())));
    }
    let wanted = format!("branch refs/heads/{}", record.target_branch);
    Ok(listed.stdout.split("\0\0").find_map(|block| {
        let mut fields = block.split('\0');
        let path = fields.next()?.strip_prefix("worktree ")?;
        fields.any(|field| field == wanted).then(|| PathBuf::from(path))
    }))
}

fn record_event(store: &RunStore, record: &RunRecord, kind: &str, payload: serde_json::Value) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(STAGE.to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}
