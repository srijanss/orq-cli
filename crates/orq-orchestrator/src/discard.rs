//! Discarding a failed run: remove its worktree and branch, release its feature locks, and forget
//! its record and logs.

use std::path::Path;

use crate::outcome::git_raw;
use crate::resume::members_of;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{RunStore, StoreError};

/// Remove the run `run_id` for good: its worktree (with any uncommitted work), its local branch,
/// its feature locks, its logs and its record. A run whose worktree is already gone keeps its
/// branch: it cannot be shown to belong to `repo`. Only a failed run can be discarded. Every check
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
    let worktree = record.worktree.to_string_lossy().into_owned();
    // Every check happens before anything is removed.
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
        let common = |dir: &Path| -> Result<Option<std::path::PathBuf>, String> {
            let out = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            Ok(out.success.then(|| std::path::PathBuf::from(out.stdout.trim())))
        };
        let (theirs, ours) = (common(&record.worktree)?, common(repo)?);
        if theirs.is_none() || ours.is_none() || theirs.and_then(|p| p.canonicalize().ok()) != ours.and_then(|p| p.canonicalize().ok()) {
            return Err(format!("{} is not the repository of {run_id}", repo.display()));
        }
    }
    if has_worktree {
        // Git allows a branch in one worktree only, and this one was verified to be on it.
        let removed = git(repo, &["worktree", "remove", "--force", "--", &worktree])?;
        if !removed.success {
            return Err(format!("git would not remove the worktree: {}", removed.stderr.trim()));
        }
        let deleted = git(repo, &["update-ref", "-d", &format!("refs/heads/{branch}")])?;
        if !deleted.success {
            return Err(format!("the branch {branch} could not be deleted: {}", deleted.stderr.trim()));
        }
    }
    // Without its worktree nothing ties the run to `repo` (another clone may share the base commit
    // and a branch name), so no branch is deleted; only the run's own records go.
    // Release each member's lock on its own, so one benign outcome cannot hide another's failure.
    for member in members_of(&record, &events) {
        match store.finish_group_run(&record.project_id, std::slice::from_ref(&member), run_id) {
            // A lock that is gone, or already held by another run, is not this run's to release.
            Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
    store.remove_run(run_id).map_err(|e| e.to_string())
}
