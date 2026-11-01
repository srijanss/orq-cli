//! After a rebase stopped on a conflict: let a harness session resolve it. If
//! the rebase is still not finished afterwards, nothing is aborted: the
//! worktree stays mid-rebase and the run stays blocked with a recovery hint.

use crate::implement::{run_session_stage, ImplementSession};
use crate::outcome::git_raw;
use crate::rebase::record_event;
use crate::store::{RunRecord, RunStore, StoreError};

const FIX_STAGE: &str = "rebase_fix";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictFixOutcome {
    /// The rebase is complete; the run can go on.
    Resolved,
    /// The rebase is still in progress; the run stays blocked.
    StillBlocked { reason: String },
}

pub fn fix_rebase_conflicts(
    store: &RunStore,
    run_id: &str,
    session: &ImplementSession<'_>,
) -> Result<ConflictFixOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !rebase_in_progress(&record)? {
        return Err(StoreError::Corrupt("the worktree is not in the middle of a rebase".to_string()));
    }
    // The commit the rebase is replaying onto, read before the session can touch it.
    let onto = rebase_onto(&record)?;
    // The session's own success is not trusted: only the repository state counts.
    run_session_stage(store, run_id, session, FIX_STAGE, 1)?;
    let still_rebasing = rebase_in_progress(&record)?;
    if !still_rebasing && completed_onto(&record, &onto)? {
        record_event(store, &record, "rebase.completed", serde_json::json!({ "resolved_by": "session", "target_sha": onto }))?;
        return Ok(ConflictFixOutcome::Resolved);
    }
    let reason = if still_rebasing {
        format!(
            "the rebase onto {} is still in progress after the fix session; resolve the conflicts in {}, \
             then run `git rebase --continue` there",
            record.target_branch,
            record.worktree.display()
        )
    } else {
        format!(
            "the fix session ended the rebase onto {} without completing it; check the branch {} in {} \
             and rebase it again",
            record.target_branch,
            record.branch,
            record.worktree.display()
        )
    };
    record_event(store, &record, "run.blocked", serde_json::json!({ "reason": reason }))?;
    Ok(ConflictFixOutcome::StillBlocked { reason })
}

/// The rebase really finished: the run branch is checked out, no entry is
/// unmerged, and the commit it was replayed onto is part of its history.
fn completed_onto(record: &RunRecord, onto: &str) -> Result<bool, StoreError> {
    let head = git_raw(&record.worktree, &["symbolic-ref", "-q", "HEAD"])?;
    if head.stdout.trim() != format!("refs/heads/{}", record.branch) {
        return Ok(false);
    }
    let unmerged = git_raw(&record.worktree, &["diff", "--name-only", "--diff-filter=U"])?;
    if !unmerged.success || !unmerged.stdout.trim().is_empty() {
        return Ok(false);
    }
    if !git_raw(&record.worktree, &["merge-base", "--is-ancestor", onto, "HEAD"])?.success {
        return Ok(false);
    }
    // The run only reaches a conflict with work of its own: a branch that sits
    // on the target with nothing past it has lost that work.
    let ahead = git_raw(&record.worktree, &["rev-list", "--count", &format!("{onto}..HEAD")])?;
    Ok(ahead.success && ahead.stdout.trim().parse::<u32>().is_ok_and(|count| count > 0))
}

fn rebase_onto(record: &RunRecord) -> Result<String, StoreError> {
    for name in ["rebase-merge/onto", "rebase-apply/onto"] {
        let path = git_raw(&record.worktree, &["rev-parse", "--git-path", name])?;
        if !path.success {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(record.worktree.join(path.stdout.trim())) {
            let sha = text.trim();
            if !sha.is_empty() {
                return Ok(sha.to_string());
            }
        }
    }
    Err(StoreError::Corrupt("could not tell which commit the rebase is replaying onto".to_string()))
}

fn rebase_in_progress(record: &RunRecord) -> Result<bool, StoreError> {
    for name in ["rebase-merge", "rebase-apply"] {
        let path = git_raw(&record.worktree, &["rev-parse", "--git-path", name])?;
        if !path.success {
            return Err(StoreError::Corrupt(format!("git rev-parse failed: {}", path.stderr.trim())));
        }
        // `--git-path` is relative to the directory git was started in.
        if record.worktree.join(path.stdout.trim()).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}
