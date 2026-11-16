//! What a run did to the ledger on its agent's behalf, recorded in the run's events.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

/// Record a `kind` event (such as `ledger.handed_back`) naming the `features` it touched.
pub fn record_ledger_features(store: &RunStore, record: &RunRecord, kind: &str, features: &[String]) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "features": features }),
    })
}
