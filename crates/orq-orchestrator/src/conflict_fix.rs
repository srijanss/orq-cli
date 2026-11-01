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
    // The session's own success is not trusted: only the repository state counts.
    run_session_stage(store, run_id, session, FIX_STAGE)?;
    if !rebase_in_progress(&record)? {
        record_event(store, &record, "rebase.completed", serde_json::json!({ "resolved_by": "session" }))?;
        return Ok(ConflictFixOutcome::Resolved);
    }
    let reason = format!(
        "the rebase onto {} is still in progress after the fix session; resolve the conflicts in {}, \
         then run `git rebase --continue` there",
        record.target_branch,
        record.worktree.display()
    );
    record_event(store, &record, "run.blocked", serde_json::json!({ "reason": reason }))?;
    Ok(ConflictFixOutcome::StillBlocked { reason })
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
