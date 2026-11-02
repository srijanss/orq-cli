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
