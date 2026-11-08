//! The runs of one project, as a list a UI or command can print: each run's feature, status,
//! models and review link, read from the run store. Another project's runs are never listed.

use crate::resume::members_of;
use crate::run_status::{summarize_run, RunPhase};
use std::path::Path;

use crate::outcome::git_raw;
use crate::store::{RunRecord, RunStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunListing {
    pub run_id: String,
    pub feature_id: String,
    /// Every feature the run is for, in order; the first is `feature_id`.
    pub members: Vec<String>,
    pub phase: RunPhase,
    pub stage: Option<String>,
    pub reason: Option<String>,
    pub implementer_model: Option<String>,
    pub reviewer_model: Option<String>,
    /// The link of the review request the run created, once it has.
    pub review_url: Option<String>,
}

/// The runs of a project, and how many run directories could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunList {
    pub runs: Vec<RunListing>,
    /// Runs whose record or event log is unreadable or corrupt. They are left out of `runs`; a
    /// record that cannot be read is counted for every project, as its owner is unknown.
    pub unreadable: usize,
}

/// Every readable run of `project_id`, ordered by run id; see [`list_runs_report`].
pub fn list_runs(store: &RunStore, project_id: &str) -> Result<Vec<RunListing>, StoreError> {
    Ok(list_runs_report(store, project_id)?.runs)
}

/// Every run of `project_id`, ordered by run id, with a count of the runs that were skipped. A
/// directory under `runs/` that is not a run is ignored; a run whose record or events cannot be
/// read is skipped and counted, so one broken run never hides the others. Only a failure to read
/// the run store itself is an error.
pub fn list_runs_report(store: &RunStore, project_id: &str) -> Result<RunList, StoreError> {
    let mut list = RunList::default();
    for run_id in store.run_ids()? {
        let record = match store.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(_) => {
                list.unreadable += 1;
                continue;
            }
        };
        if record.project_id != project_id {
            continue;
        }
        let Ok(events) = store.events(&run_id) else {
            list.unreadable += 1;
            continue;
        };
        let summary = summarize_run(&events);
        let review_url = events
            .iter()
            .rev()
            .find(|e| e.kind == "review.created" || e.kind == "review.adopted")
            .and_then(|e| e.payload.get("url")?.as_str().map(str::to_string));
        list.runs.push(RunListing {
            run_id,
            feature_id: record.feature_id.clone(),
            members: members_of(&record, &events),
            phase: summary.phase,
            stage: summary.stage,
            reason: summary.reason,
            implementer_model: record.profiles.get("implementer").map(|p| p.model.clone()),
            reviewer_model: record.profiles.get("reviewer").map(|p| p.model.clone()),
            review_url,
        });
    }
    Ok(list)
}

/// Show as [`RunPhase::Merged`] each failed or review-waiting run of `list` whose work is on its
/// target branch in `repo` (see [`work_is_on_target`]). A run whose record or branch cannot be
/// read is left as it is.
pub fn mark_merged_runs(store: &RunStore, list: &mut RunList, repo: &Path) {
    for run in &mut list.runs {
        if !matches!(run.phase, RunPhase::Failed | RunPhase::NeedsReview) {
            continue;
        }
        if store.load(&run.run_id).is_ok_and(|record| work_is_on_target(repo, &record)) {
            run.phase = RunPhase::Merged;
        }
    }
}

/// Whether the run's work was merged into its target branch: its local branch in `repo` has at
/// least one commit past the run's base, and every one of them is reachable from the local target
/// branch. A run with no commits, a branch or base git cannot resolve, or a git that fails, is not.
pub fn work_is_on_target(repo: &Path, record: &RunRecord) -> bool {
    work_on_target(repo, record).unwrap_or(false)
}

/// [`work_is_on_target`], but a git that fails to answer is an error rather than "not merged": only
/// a run whose branch, base or target branch does not exist is not merged.
pub(crate) fn work_on_target(repo: &Path, record: &RunRecord) -> Result<bool, String> {
    if record.branch.is_empty() || record.base_sha.is_empty() || record.target_branch.is_empty() {
        return Ok(false);
    }
    let branch = format!("refs/heads/{}", record.branch);
    let target = format!("refs/heads/{}", record.target_branch);
    let failed = |e: String| format!("could not tell whether {}'s work is on its target branch {}: {e}", record.run_id, record.target_branch);
    for rev in [&branch, &record.base_sha, &target] {
        let out = git_raw(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{rev}^{{commit}}")]).map_err(|e| failed(e.to_string()))?;
        // With --quiet, a revision that does not exist exits 1 and says nothing; anything else is git failing.
        match out.code {
            Some(0) => {}
            Some(1) => return Ok(false),
            _ => return Err(failed(out.stderr.trim().to_string())),
        }
    }
    // Whether the branch has any commit `not` lacks: git stops at the first one it finds.
    let has_commit_past = |not: &str| -> Result<bool, String> {
        let out = git_raw(repo, &["rev-list", "--max-count=1", "--end-of-options", &branch, &format!("^{not}")]).map_err(|e| failed(e.to_string()))?;
        if !out.success {
            return Err(failed(out.stderr.trim().to_string()));
        }
        Ok(!out.stdout.trim().is_empty())
    };
    Ok(has_commit_past(&record.base_sha)? && !has_commit_past(&target)?)
}
