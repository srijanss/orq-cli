//! Discarding a failed run: remove its worktree and branch, release its feature locks, and forget
//! its record and logs.

use std::path::Path;

use crate::outcome::git_raw;
use crate::resume::members_of;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{RunStore, StoreError};

/// Remove the run `run_id` for good: its worktree (with any uncommitted work), its local branch,
/// its feature locks, its logs and its record. Only a failed run can be discarded. Every check
/// happens before anything is removed, and the record goes last, so a failure part-way can be
/// retried. `repo` is the repository the run's worktree belongs to.
pub fn discard_run(store: &RunStore, run_id: &str, repo: &Path) -> Result<(), String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if phase != RunPhase::Failed {
        return Err(format!("{run_id} is {}; only a failed run can be discarded", phase.as_str()));
    }
    let branch = record.branch.as_str();
    if branch.is_empty() || branch == record.target_branch {
        return Err(format!("{run_id} records the branch {branch:?}, which must not be deleted"));
    }
    let git = |dir: &Path, args: &[&str]| git_raw(dir, args).map_err(|e: StoreError| e.to_string());
    let has_worktree = record.worktree.exists();
    if has_worktree {
        let same_as_repo = match (record.worktree.canonicalize(), repo.canonicalize()) {
            (Ok(worktree), Ok(repo)) => worktree == repo,
            _ => true,
        };
        if same_as_repo {
            return Err(format!("the worktree of {run_id} is the repository itself; refusing to remove it"));
        }
        let head = git(&record.worktree, &["symbolic-ref", "--quiet", "HEAD"])?;
        if !head.success || head.stdout.trim() != format!("refs/heads/{branch}") {
            return Err(format!("the worktree of {run_id} is not on its branch {branch}; refusing to remove it"));
        }
        let worktree = record.worktree.to_string_lossy().into_owned();
        let removed = git(repo, &["worktree", "remove", "--force", "--", &worktree])?;
        if !removed.success {
            return Err(format!("git would not remove the worktree: {}", removed.stderr.trim()));
        }
    }
    git(repo, &["worktree", "prune"])?;
    let deleted = git(repo, &["update-ref", "-d", &format!("refs/heads/{branch}")])?;
    if !deleted.success {
        return Err(format!("the branch {branch} could not be deleted: {}", deleted.stderr.trim()));
    }
    let members = members_of(&record, &events);
    match store.finish_group_run(&record.project_id, &members, run_id) {
        // A lock that is gone, or already held by another run, is not this run's to release.
        Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
        Err(err) => return Err(err.to_string()),
    }
    store.remove_run(run_id).map_err(|e| e.to_string())
}
