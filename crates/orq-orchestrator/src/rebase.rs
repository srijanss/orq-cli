//! Before the push: bring the run branch up to date with the target branch.
//! A conflict is never resolved or aborted here: the worktree stays mid-rebase
//! so the next step (or a person) can finish it.

use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "rebase";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// The run branch now sits on this commit of the target branch.
    Rebased { target_sha: String },
    /// Rebasing is switched off for this run.
    Skipped,
    /// The rebase stopped on conflicts; the run is blocked.
    Conflict { reason: String },
}

pub fn rebase_onto_target(store: &RunStore, run_id: &str, enabled: bool) -> Result<RebaseOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !enabled {
        record_event(store, &record, "rebase.skipped", serde_json::json!({}))?;
        return Ok(RebaseOutcome::Skipped);
    }
    let worktree = record.worktree.as_path();
    // Only ever rewrite the run's own branch: a switched or detached HEAD is left alone.
    let head = git_raw(worktree, &["symbolic-ref", "-q", "HEAD"])?;
    if head.stdout.trim() != format!("refs/heads/{}", record.branch) {
        return Err(StoreError::Corrupt(format!("the worktree is not on the run branch {}", record.branch)));
    }
    let refspec = format!("refs/heads/{}", record.target_branch);
    let fetched = git_raw(worktree, &["fetch", "--no-tags", "--", &record.remote, &refspec])?;
    if !fetched.success {
        return Err(StoreError::Corrupt(format!(
            "could not fetch {} from the remote: {}",
            record.target_branch,
            fetched.stderr.replace(&record.remote, "<remote>").trim()
        )));
    }
    let target_sha = git_raw(worktree, &["rev-parse", "--verify", "FETCH_HEAD^{commit}"])?;
    if !target_sha.success {
        return Err(StoreError::Corrupt("the fetched target branch has no commit".to_string()));
    }
    let target_sha = target_sha.stdout.trim().to_string();

    let rebased = git_raw(worktree, &["rebase", &target_sha])?;
    if rebased.success {
        record_event(store, &record, "rebase.completed", serde_json::json!({ "target_sha": target_sha }))?;
        return Ok(RebaseOutcome::Rebased { target_sha });
    }
    let unmerged = git_raw(worktree, &["diff", "--name-only", "--diff-filter=U"])?;
    if unmerged.stdout.trim().is_empty() {
        return Err(StoreError::Corrupt(format!("git rebase failed: {}", rebased.stderr.trim())));
    }
    let reason = format!("the rebase onto {} stopped on a conflict", record.target_branch);
    record_event(store, &record, "run.blocked", serde_json::json!({ "reason": reason }))?;
    Ok(RebaseOutcome::Conflict { reason })
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
