//! The runs of one project, as a list a UI or command can print: each run's feature, status,
//! models and review link, read from the run store. Another project's runs are never listed.

use crate::run_status::{summarize_run, RunPhase};
use crate::store::{RunStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunListing {
    pub run_id: String,
    pub feature_id: String,
    pub phase: RunPhase,
    pub stage: Option<String>,
    pub reason: Option<String>,
    pub implementer_model: Option<String>,
    pub reviewer_model: Option<String>,
    /// The link of the review request the run created, once it has.
    pub review_url: Option<String>,
}

/// Every run of `project_id`, ordered by run id. A directory under `runs/` that is not a run
/// (no readable record) is skipped; a run whose record or events are corrupt is an error.
pub fn list_runs(store: &RunStore, project_id: &str) -> Result<Vec<RunListing>, StoreError> {
    let mut listings = Vec::new();
    for run_id in store.run_ids()? {
        let record = match store.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(err) => return Err(err),
        };
        if record.project_id != project_id {
            continue;
        }
        let events = store.events(&run_id)?;
        let summary = summarize_run(&events);
        let review_url = events
            .iter()
            .rev()
            .find(|e| e.kind == "review.created" || e.kind == "review.adopted")
            .and_then(|e| e.payload.get("url")?.as_str().map(str::to_string));
        listings.push(RunListing {
            run_id,
            feature_id: record.feature_id.clone(),
            phase: summary.phase,
            stage: summary.stage,
            reason: summary.reason,
            implementer_model: record.profiles.get("implementer").map(|p| p.model.clone()),
            reviewer_model: record.profiles.get("reviewer").map(|p| p.model.clone()),
            review_url,
        });
    }
    Ok(listings)
}
