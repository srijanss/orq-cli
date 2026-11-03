//! Finding the runs whose work is over and whose local leftovers can go: the worktree and the
//! local branch of a run whose review request was merged or closed. Everything is fail-closed:
//! a run is reaped only when every check passes, and anything uncertain is kept with a reason.
//! The remote branch and the review itself are never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_core::review_lifecycle::ProviderState;

use crate::outcome::git_raw;
use crate::review_observe::observe_review;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{RunStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapVerdict {
    Reap,
    Keep(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapItem {
    pub run_id: String,
    pub feature_id: String,
    pub branch: String,
    pub worktree: PathBuf,
    /// The commit the branch pointed at when it was judged; the branch is only deleted if it still does.
    pub tip: String,
    pub verdict: ReapVerdict,
}

fn keep(reason: &str) -> ReapVerdict {
    ReapVerdict::Keep(reason.to_string())
}

/// Judge every readable run of `project_id`. `pinned` run ids are always kept. `program_for`
/// maps a provider ("github", "gitlab") to its CLI.
pub fn plan_reap(
    store: &RunStore,
    repo: &Path,
    project_id: &str,
    pinned: &[String],
    program_for: &dyn Fn(&str) -> Option<PathBuf>,
    timeout: Duration,
) -> Result<Vec<ReapItem>, StoreError> {
    let mut items = Vec::new();
    for run_id in store.run_ids()? {
        let Ok(record) = store.load(&run_id) else { continue };
        if record.project_id != project_id {
            continue;
        }
        let mut item = ReapItem {
            run_id: run_id.clone(),
            feature_id: record.feature_id.clone(),
            branch: record.branch.clone(),
            worktree: record.worktree.clone(),
            tip: String::new(),
            verdict: ReapVerdict::Reap,
        };
        item.verdict = judge(store, repo, pinned, program_for, timeout, &record, &mut item.tip)?;
        items.push(item);
    }
    Ok(items)
}

fn judge(
    store: &RunStore,
    repo: &Path,
    pinned: &[String],
    program_for: &dyn Fn(&str) -> Option<PathBuf>,
    timeout: Duration,
    record: &crate::store::RunRecord,
    tip: &mut String,
) -> Result<ReapVerdict, StoreError> {
    if pinned.iter().any(|p| p == &record.run_id) {
        return Ok(keep("the run is pinned"));
    }
    let Ok(events) = store.events(&record.run_id) else { return Ok(keep("the run's events cannot be read")) };
    if summarize_run(&events).phase == RunPhase::Running {
        return Ok(keep("the run is still running"));
    }
    if !within_worktrees_dir(repo, &record.worktree) {
        return Ok(keep("the worktree is not under .claude/worktrees of the repository"));
    }
    let branch_ref = format!("refs/heads/{}", record.branch);
    let resolved = git_raw(repo, &["rev-parse", "--verify", "--quiet", &format!("{branch_ref}^{{commit}}")])?;
    if !resolved.success {
        return Ok(keep("the local branch cannot be found"));
    }
    *tip = resolved.stdout.trim().to_string();
    let Some(program) = program_for(&record.provider) else { return Ok(keep("no provider CLI to ask about the review")) };
    let observed = match observe_review(store, &record.run_id, &program, timeout)? {
        Ok(observed) => observed,
        Err(error) => return Ok(ReapVerdict::Keep(format!("the review could not be checked: {}", error.describe()))),
    };
    if observed.observation.state == ProviderState::Open {
        return Ok(keep("the review is not merged or closed"));
    }
    if observed.observation.head_sha != *tip {
        return Ok(keep("the branch has commits that were not pushed to the review"));
    }
    let status = git_raw(&record.worktree, &["status", "--porcelain", "--untracked-files=all", "--ignored"])?;
    if !status.success {
        return Ok(keep("the worktree state could not be read"));
    }
    if !status.stdout.trim().is_empty() {
        return Ok(keep("the worktree has uncommitted changes"));
    }
    Ok(ReapVerdict::Reap)
}

fn within_worktrees_dir(repo: &Path, worktree: &Path) -> bool {
    let (Ok(repo), Ok(worktree)) = (repo.canonicalize(), worktree.canonicalize()) else { return false };
    worktree != repo && worktree.starts_with(repo.join(".claude").join("worktrees")) && worktree != repo.join(".claude").join("worktrees")
}

/// Remove the worktree and the local branch of a run judged [`ReapVerdict::Reap`]. Does nothing
/// for any other verdict. The branch goes only if it still points at the judged commit.
pub fn reap_run(repo: &Path, item: &ReapItem) -> Result<(), StoreError> {
    if item.verdict != ReapVerdict::Reap || item.tip.is_empty() {
        return Ok(());
    }
    let worktree = item.worktree.to_string_lossy().into_owned();
    let removed = git_raw(repo, &["worktree", "remove", "--", &worktree])?;
    if !removed.success {
        return Err(StoreError::Corrupt(format!("git would not remove the worktree: {}", removed.stderr.trim())));
    }
    let deleted = git_raw(repo, &["update-ref", "-d", &format!("refs/heads/{}", item.branch), &item.tip])?;
    if !deleted.success {
        return Err(StoreError::Corrupt(format!("the branch could not be deleted: {}", deleted.stderr.trim())));
    }
    Ok(())
}
