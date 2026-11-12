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
    let fast_forward = git_raw(&record.worktree, &["merge-base", "--is-ancestor", &target, &head_sha])?;
    if !fast_forward.success {
        return Ok(MergeOutcome::Refused {
            reason: format!("{} moved since this run rebased onto it, so it cannot be fast-forwarded to this run's branch; the branch and worktree are kept", record.target_branch),
        });
    }
    match checkout_of(&record)? {
        Some(checkout) => {
            let status = git_raw(&checkout, &["status", "--porcelain", "--untracked-files=no"])?;
            if !status.stdout.trim().is_empty() {
                return Ok(MergeOutcome::Refused {
                    reason: format!("{} is checked out at {} with uncommitted changes; commit or stash them, then resume this run", record.target_branch, checkout.display()),
                });
            }
            let merged = git_raw(&checkout, &["merge", "--ff-only", "--quiet", &head_sha])?;
            if !merged.success {
                return Err(StoreError::Corrupt(format!("git merge --ff-only failed: {}", merged.stderr.trim())));
            }
        }
        // With no checkout to update, only the ref moves, and only from the commit the run
        // rebased onto (or started from), so a base branch that moved since is never overwritten.
        None => {
            let expected = crate::resume::rebased_onto(store, run_id)?.unwrap_or_else(|| record.base_sha.clone());
            let reason = format!("orq: merge-local {}", record.run_id);
            let moved = git_raw(&record.worktree, &["update-ref", "-m", &reason, &target, &head_sha, &expected])?;
            if !moved.success {
                return Err(StoreError::Corrupt(format!("git update-ref failed: {}", moved.stderr.trim())));
            }
        }
    }
    record_event(store, &record, "merge.completed", serde_json::json!({ "head_sha": head_sha, "target_branch": record.target_branch }))?;
    Ok(MergeOutcome::Merged { head_sha })
}

/// The working tree that has the target branch checked out, if any does.
fn checkout_of(record: &RunRecord) -> Result<Option<PathBuf>, StoreError> {
    let listed = git_raw(&record.worktree, &["worktree", "list", "--porcelain"])?;
    if !listed.success {
        return Err(StoreError::Corrupt(format!("cannot list the repository's worktrees: {}", listed.stderr.trim())));
    }
    let wanted = format!("branch refs/heads/{}", record.target_branch);
    Ok(listed.stdout.split("\n\n").find_map(|block| {
        let mut lines = block.lines();
        let path = lines.next()?.strip_prefix("worktree ")?;
        lines.any(|line| line == wanted).then(|| PathBuf::from(path))
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
