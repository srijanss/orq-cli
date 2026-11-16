//! A person's action on a run's review, recorded in the run's events, and the runs whose events show
//! they were merged into their base locally.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

/// Record a `kind` event (such as `review.dismissed`) for the review of `head_sha`, taken by `by`.
pub fn record_review_action(store: &RunStore, run_id: &str, kind: &str, head_sha: &str, by: &str) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "head_sha": head_sha, "by": by }),
    })
}

/// Every run whose events record a local merge, with the commit it was merged at (the newest, if more
/// than one). A run whose events cannot be read is passed over: it is no evidence of a merge.
pub fn local_merges(store: &RunStore) -> Result<Vec<(String, String)>, StoreError> {
    let mut merges = Vec::new();
    for run_id in store.run_ids()? {
        let Ok(events) = store.events(&run_id) else { continue };
        let merged = events.iter().rev().filter(|e| e.kind == "merge.completed").find_map(|e| e.payload["head_sha"].as_str().map(str::to_string));
        if let Some(head_sha) = merged {
            merges.push((run_id, head_sha));
        }
    }
    Ok(merges)
}
