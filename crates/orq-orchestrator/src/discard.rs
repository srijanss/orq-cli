//! Discarding a failed or blocked run: remove its worktree and branch, release its feature locks, and forget
//! its record and logs.

use std::path::Path;

use crate::outcome::git_raw;
use crate::resume::members_of;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Claim, RunStore, StoreError};

/// Remove the run `run_id` for good: its worktree (with any uncommitted work), its local branch,
/// its feature locks, its logs and its record. A run whose worktree is already gone keeps its
/// branch: it cannot be shown to belong to `repo`. Only a failed or blocked run, which has ended, can be discarded. Every check
/// happens before anything is removed, and the record goes last, so a failure part-way can be
/// retried. `repo` is the repository the run's worktree belongs to. Returns the run's features.
pub fn discard_run(store: &RunStore, run_id: &str, repo: &Path) -> Result<Vec<String>, String> {
    discard_run_then(store, run_id, repo, &mut |_, _| {})
}

/// [`discard_run`], calling `orphaned(project_id, features)` once the worktree is gone and before any
/// lock is released, with the run's features that no other run holds: nothing continues those, and
/// no new run can take them while the call runs.
pub fn discard_run_then(store: &RunStore, run_id: &str, repo: &Path, orphaned: &mut dyn FnMut(&str, &[String])) -> Result<Vec<String>, String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if !matches!(phase, RunPhase::Failed | RunPhase::Blocked) {
        return Err(format!("{run_id} is {}; only a failed or blocked run can be discarded", phase.as_str()));
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
        // `worktree add --force` lets several worktrees share a branch; deleting it would leave the
        // others on an unborn HEAD, so a branch checked out anywhere else is never deleted.
        let listing = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
        if !listing.success {
            return Err(format!("cannot list the worktrees of {}: {}", repo.display(), listing.stderr.trim()));
        }
        let ours = record.worktree.canonicalize().map_err(|e| e.to_string())?;
        let wanted = format!("branch refs/heads/{branch}");
        let mut path: Option<String> = None;
        for field in listing.stdout.split('\0') {
            if let Some(p) = field.strip_prefix("worktree ") {
                path = Some(p.to_string());
            } else if field == wanted {
                let other = path.as_deref().unwrap_or_default();
                if Path::new(other).canonicalize().ok().as_ref() != Some(&ours) {
                    return Err(format!("the branch {branch} is checked out in {other}; refusing to delete it"));
                }
            }
        }
    }
    if has_worktree {
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
    let members = members_of(&record, &events);
    // A member whose lock is free is claimed first, so no new run can start it while it is reopened.
    // If one cannot be claimed, the locks taken here are given back (the run's own stay, and so
    // does the record), so no feature is left blocked by a run that is not doing it.
    let mut ours = Vec::new();
    let mut taken = Vec::new();
    for member in &members {
        match store.claim_lock(&record.project_id, member, run_id) {
            Ok(Claim::Taken) => taken.push(member.clone()),
            Ok(Claim::AlreadyHeld) => {}
            Ok(Claim::HeldByAnother) => continue,
            Err(err) => {
                // Best effort: the error being returned is the one that matters.
                let _ = store.finish_group_run(&record.project_id, &taken, run_id);
                return Err(err.to_string());
            }
        }
        ours.push(member.clone());
    }
    if !ours.is_empty() {
        orphaned(&record.project_id, &ours);
    }
    for member in &members {
        match store.finish_group_run(&record.project_id, std::slice::from_ref(member), run_id) {
            // A lock that is gone, or already held by another run, is not this run's to release.
            Ok(()) | Err(StoreError::NotFound(_)) | Err(StoreError::FeatureBusy { .. }) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
    store.remove_run(run_id).map_err(|e| e.to_string())?;
    Ok(members)
}
