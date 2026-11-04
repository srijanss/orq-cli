//! Removing a run's worktree once the run has finished well.

use std::path::Path;

use crate::ci_wait::CiOutcome;
use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "cleanup";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// The worktree directory and its registration are gone.
    Removed,
    /// The worktree stays where it is, for this reason.
    Kept { reason: String },
}

/// Remove the worktree of `run_id` if the run reached `review_ready` with CI green (or
/// CI disabled). A blocked or failed run, a red or unknown CI result, and a worktree
/// with uncommitted work are all kept. Only the worktree goes: the branch and the
/// pushed review request are never touched.
pub fn cleanup_worktree(
    store: &RunStore,
    run_id: &str,
    review_ready: bool,
    ci: &CiOutcome,
) -> Result<CleanupOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !review_ready {
        return keep(store, &record, "the run did not reach review_ready".to_string(), false);
    }
    if !matches!(ci, CiOutcome::Passed | CiOutcome::Skipped) {
        return keep(store, &record, "CI did not pass".to_string(), false);
    }
    // Anything uncommitted is work that removal would lose: untracked files, and ignored
    // ones too, since `git worktree remove` deletes ignored files without --force.
    match uncommitted_work(&record.worktree)? {
        Some(UncommittedWork::Unreadable) => return keep(store, &record, "the worktree state could not be read".to_string(), true),
        Some(UncommittedWork::Present) => return keep(store, &record, "the worktree has uncommitted changes".to_string(), true),
        None => {}
    }
    let common = git_raw(&record.worktree, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if !common.success {
        return keep(store, &record, "the repository of the worktree could not be found".to_string(), true);
    }
    let worktree = record.worktree.to_string_lossy().into_owned();
    // Run from the repository's own git dir: the worktree being removed cannot be the working directory.
    // No --force: git itself refuses a locked or modified worktree.
    let removed = git_raw(Path::new(common.stdout.trim()), &["worktree", "remove", "--", &worktree])?;
    if !removed.success {
        return keep(store, &record, format!("git would not remove the worktree: {}", removed.stderr.trim()), true);
    }
    record_event(store, &record, "worktree.removed", serde_json::json!({ "worktree": worktree }))?;
    Ok(CleanupOutcome::Removed)
}

/// Ignored paths orq itself puts into a run's worktree; they are not the agent's work.
const ORQ_WRITTEN: [&str; 5] = [".mcp.json", ".mcpctl.toml", ".mcpctl.lock", ".claude/settings.local.json", ".claude/"];

pub(crate) enum UncommittedWork {
    Unreadable,
    Present,
}

/// Whether `worktree` holds anything uncommitted besides the ignored files orq wrote itself.
pub(crate) fn uncommitted_work(worktree: &Path) -> Result<Option<UncommittedWork>, StoreError> {
    let status = git_raw(worktree, &["status", "--porcelain", "--untracked-files=all", "--ignored"])?;
    if !status.success {
        return Ok(Some(UncommittedWork::Unreadable));
    }
    let own = |line: &str| {
        line.strip_prefix("!! ").is_some_and(|path| {
            let path = path.trim_matches('"');
            path == concat!(".", "env") || ORQ_WRITTEN.contains(&path)
        })
    };
    Ok(status.stdout.lines().any(|line| !line.trim().is_empty() && !own(line)).then_some(UncommittedWork::Present))
}

/// `run_succeeded` says whether the run itself finished well, apart from why the worktree stays.
fn keep(store: &RunStore, record: &RunRecord, reason: String, run_succeeded: bool) -> Result<CleanupOutcome, StoreError> {
    let worktree = record.worktree.to_string_lossy().into_owned();
    record_event(store, record, "worktree.kept", serde_json::json!({ "worktree": worktree, "reason": reason, "run_succeeded": run_succeeded }))?;
    Ok(CleanupOutcome::Kept { reason })
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
