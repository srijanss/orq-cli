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
    if let Some(problem) = worktree_problem(repo, &record.worktree, &record.branch, tip)? {
        return Ok(ReapVerdict::Keep(problem));
    }
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
    if let Some(problem) = uncommitted_problem(&record.worktree)? {
        return Ok(ReapVerdict::Keep(problem));
    }
    Ok(ReapVerdict::Reap)
}

/// Anything uncommitted, ignored files included, is work that removal would lose.
fn uncommitted_problem(worktree: &Path) -> Result<Option<String>, StoreError> {
    use crate::cleanup::{uncommitted_work, UncommittedWork};
    Ok(match uncommitted_work(worktree)? {
        Some(UncommittedWork::Unreadable) => Some("the worktree state could not be read".to_string()),
        Some(UncommittedWork::Present) => Some("the worktree has uncommitted changes".to_string()),
        None => None,
    })
}

/// Why the worktree is not, verifiably, the checkout of `branch` at `tip` of this repository.
/// Removing a worktree on another commit or branch could discard the only copy of its commits.
fn worktree_problem(repo: &Path, worktree: &Path, branch: &str, tip: &str) -> Result<Option<String>, StoreError> {
    if let Some(problem) = checkout_problem(repo, worktree, branch)? {
        return Ok(Some(problem));
    }
    let at = git_raw(worktree, &["rev-parse", "--verify", "HEAD"])?;
    if !at.success || at.stdout.trim() != tip {
        return Ok(Some("the worktree is not on the branch tip".to_string()));
    }
    Ok(None)
}

/// Why the worktree is not, verifiably, a checkout of `branch` belonging to this repository.
pub(crate) fn checkout_problem(repo: &Path, worktree: &Path, branch: &str) -> Result<Option<String>, StoreError> {
    let common = |dir: &Path| -> Result<Option<PathBuf>, StoreError> {
        let out = git_raw(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(out.success.then(|| PathBuf::from(out.stdout.trim())).and_then(|p| p.canonicalize().ok()))
    };
    match (common(repo)?, common(worktree)?) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Ok(Some("the worktree does not belong to this repository".to_string())),
    }
    let head = git_raw(worktree, &["symbolic-ref", "--quiet", "HEAD"])?;
    if !head.success || head.stdout.trim() != format!("refs/heads/{branch}") {
        return Ok(Some("the worktree is not on the branch of the run".to_string()));
    }
    Ok(None)
}

/// Drops the test databases a reaped run's branch gave its worktree, the way the end of a run does:
/// the names are derived again from the main checkout's dotenv file and the run settings, then
/// `test_db_drop_cmd` is run for each. Only notes come back; nothing here fails a reap.
pub fn reap_run_databases(repo: &Path, branch: &str, settings: &crate::RunSettings) -> Vec<String> {
    let named = settings.test_db_vars.is_some();
    let vars = settings.test_db_vars.clone().unwrap_or_else(|| vec!["DB_NAME".to_string(), "B2C_DB_NAME".to_string()]);
    let databases = crate::derive_worktree_databases_with(repo, &vars, branch, named, settings.db_name_template.as_deref());
    let mut notes = databases.notes;
    notes.extend(crate::drop_test_databases(repo, &databases.names, settings.test_db_drop_cmd.as_deref(), settings.keep_test_db.unwrap_or(false)));
    notes
}

pub(crate) fn within_worktrees_dir(repo: &Path, worktree: &Path) -> bool {
    let (Ok(repo), Ok(worktree)) = (repo.canonicalize(), worktree.canonicalize()) else { return false };
    worktree != repo && worktree.starts_with(repo.join(".claude").join("worktrees")) && worktree != repo.join(".claude").join("worktrees")
}

/// Remove the worktree and the local branch of a run judged [`ReapVerdict::Reap`]. Does nothing
/// for any other verdict. The branch goes only if it still points at the judged commit.
pub fn reap_run(repo: &Path, item: &ReapItem) -> Result<(), StoreError> {
    if item.verdict != ReapVerdict::Reap || item.tip.is_empty() {
        return Ok(());
    }
    // The plan may be minutes old: look again, right before anything is removed.
    let current = git_raw(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}^{{commit}}", item.branch)])?;
    if !current.success || current.stdout.trim() != item.tip {
        return Err(StoreError::Corrupt("the branch moved since the plan was made".to_string()));
    }
    if let Some(problem) = worktree_problem(repo, &item.worktree, &item.branch, &item.tip)?.or(uncommitted_problem(&item.worktree)?) {
        return Err(StoreError::Corrupt(problem));
    }
    // A rebase that died between its fetch and its cleanup leaves the run's private ref; nothing else uses it.
    // It goes first: if it cannot be deleted the run is still whole and the reap can be retried.
    let private = crate::rebase::private_ref_name(&item.run_id);
    let removed_ref = git_raw(repo, &["update-ref", "-d", &private])?;
    if !removed_ref.success {
        return Err(StoreError::Corrupt(format!("the private ref {private} could not be deleted, nothing else was removed: {}", removed_ref.stderr.trim())));
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

/// What no run uses any more: private rebase refs and the branches whose test databases may remain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Orphans {
    /// Private rebase refs (`refs/orq/rebase/...`) that no run in a live state owns.
    pub refs: Vec<String>,
    /// `(run id, branch)` of finished runs whose worktree is gone, so nothing uses their test databases.
    pub database_branches: Vec<(String, String)>,
}

/// A run keeps its refs and databases while it is running or its worktree is still there.
fn in_use(store: &RunStore, record: &crate::store::RunRecord) -> bool {
    // Only a confirmed absence frees the run's leftovers; a failed check keeps them.
    if record.worktree.try_exists().unwrap_or(true) {
        return true;
    }
    match store.events(&record.run_id) {
        Ok(events) => summarize_run(&events).phase == RunPhase::Running,
        // Unreadable events: assume it is in use.
        Err(_) => true,
    }
}

/// Find the leftovers that no run owns. Anything uncertain counts as owned.
pub fn plan_orphans(store: &RunStore, repo: &Path, project_id: &str) -> Result<Orphans, StoreError> {
    let mut owned_refs = std::collections::BTreeSet::new();
    let mut used_branches = std::collections::BTreeSet::new();
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut unreadable = false;
    for run_id in store.run_ids()? {
        let Ok(record) = store.load(&run_id) else {
            // A record that cannot be read still keeps its ref, and the branch it used is unknown.
            owned_refs.insert(crate::rebase::private_ref_name(&run_id));
            unreadable = true;
            continue;
        };
        let live = in_use(store, &record);
        if live {
            owned_refs.insert(crate::rebase::private_ref_name(&record.run_id));
        }
        if record.project_id == project_id {
            if live {
                used_branches.insert(record.branch.clone());
            } else {
                candidates.push((record.run_id.clone(), record.branch.clone()));
            }
        }
    }
    let listed = git_raw(repo, &["for-each-ref", "--format=%(refname)", "refs/orq/rebase/"])?;
    if !listed.success {
        return Err(StoreError::Corrupt(format!("the private refs could not be listed: {}", listed.stderr.trim())));
    }
    let refs = listed.stdout.lines().map(str::trim).filter(|name| !name.is_empty() && !owned_refs.contains(*name)).map(str::to_string).collect();
    let mut seen = std::collections::BTreeSet::new();
    // With a record unreadable, no branch is known to be unused.
    let candidates = if unreadable { Vec::new() } else { candidates };
    let database_branches = candidates.into_iter().filter(|(_, branch)| !used_branches.contains(branch) && seen.insert(branch.clone())).collect();
    Ok(Orphans { refs, database_branches })
}

/// Delete one orphaned private rebase ref. Anything else is refused.
pub fn reap_orphan_ref(repo: &Path, name: &str) -> Result<(), StoreError> {
    if !name.starts_with("refs/orq/rebase/") {
        return Err(StoreError::Corrupt(format!("{name} is not a private rebase ref")));
    }
    let removed = git_raw(repo, &["update-ref", "--no-deref", "-d", name])?;
    if !removed.success {
        return Err(StoreError::Corrupt(format!("the private ref {name} could not be deleted: {}", removed.stderr.trim())));
    }
    Ok(())
}
