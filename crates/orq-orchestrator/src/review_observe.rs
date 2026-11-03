//! Looking at a run's review request on the provider, read-only: one listing
//! of the reviews for the run's branch pair, reconciled with the review the
//! run recorded. Nothing here can change a review.

use std::path::Path;
use std::time::Duration;

use orq_core::provider_observation::{classify_failure, list_command, parse_list};
use orq_core::review_lifecycle::{reconcile, Candidate, Observation, ObservationError};

use crate::review_draft::{run_captured, DraftSession};
use crate::review_request::{provider_environment, recorded_review, repository_url};
use crate::store::{RunStore, StoreError};

/// The review a run tracks, as the provider shows it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub observation: Observation,
    /// The review that is tracked.
    pub review: Candidate,
    /// Other reviews for the same branches, which are left alone.
    pub duplicates: Vec<Candidate>,
}

/// Observe the review of `run_id` with the provider CLI at `program`. The outer error is a local
/// problem (no such run, no pinned repository); the inner one is the provider's.
pub fn observe_review(
    store: &RunStore,
    run_id: &str,
    program: &Path,
    timeout: Duration,
) -> Result<Result<Observed, ObservationError>, StoreError> {
    let record = store.load(run_id)?;
    if record.provider != "github" && record.provider != "gitlab" {
        return Err(StoreError::Corrupt(format!("the provider {} has no review requests to observe", record.provider)));
    }
    let repository = repository_url(&record)?;
    let (cli, args) = list_command(&record.provider, &repository, &record.branch, &record.target_branch).map_err(StoreError::Corrupt)?;
    let recorded = recorded_review(store, run_id)?;

    let session = DraftSession { program: program.to_string_lossy().into_owned(), args, env: provider_environment(), cwd: record.worktree.clone() };
    debug_assert!(cli == "gh" || cli == "glab");
    let text = match run_captured(&session, timeout) {
        Ok(text) => text,
        Err(failure) if failure.stderr.trim().is_empty() => {
            return Ok(Err(ObservationError::Unavailable(orq_core::redact::redact(&failure.reason))));
        }
        Err(failure) => return Ok(Err(classify_failure(&failure.stderr))),
    };
    let listed = match parse_list(&record.provider, text.trim()) {
        Ok(listed) => listed,
        Err(error) => return Ok(Err(error)),
    };
    let candidates: Vec<Candidate> = listed.iter().map(|l| l.candidate.clone()).collect();
    let reconciled = reconcile(&candidates, recorded.as_ref().map(|r| r.id.as_str()));
    let Some(review) = reconciled.primary else {
        return Ok(Err(ObservationError::Unavailable("no review request was found for the run's branch".to_string())));
    };
    let head_sha = listed.iter().find(|l| l.candidate.review_id == review.review_id).map(|l| l.head_sha.clone()).unwrap_or_default();
    Ok(Ok(Observed { observation: Observation { state: review.state, head_sha }, review, duplicates: reconciled.duplicates }))
}
