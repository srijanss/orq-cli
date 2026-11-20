//! The ledger features a run's fix sessions started (gate, CI or conflict fixes). They are the run's
//! own, so it closes them when it ends: `completed` when it delivered, `abandoned` when discarded.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

pub(crate) const ADDED: &str = "fix.features_added";

/// Record that the fix session `session` (its stage, or `carried` for those a resumed run took
/// over) added `features` to the ledger.
pub fn record_fix_features_added(
    store: &RunStore,
    record: &RunRecord,
    session: &str,
    features: &[String],
) -> Result<(), StoreError> {
    append_added(
        store,
        record,
        serde_json::json!({ "session": session, "features": features }),
    )
}

fn append_added(
    store: &RunStore,
    record: &RunRecord,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: ADDED.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}

/// Every feature `run_id`'s fix sessions added, once each, in the order they were first recorded.
pub fn fix_features(store: &RunStore, run_id: &str) -> Result<Vec<String>, StoreError> {
    let mut features: Vec<String> = Vec::new();
    for event in store.events(run_id)?.iter().filter(|e| e.kind == ADDED) {
        let added = event
            .payload
            .get("features")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten();
        for name in added.filter_map(|n| n.as_str()) {
            if !features.iter().any(|f| f == name) {
                features.push(name.to_string());
            }
        }
    }
    Ok(features)
}

/// Carry the features `from_run_id`'s fix sessions added into the run that resumes it, so it closes
/// them when it ends. Nothing is recorded when there are none.
pub fn carry_fix_features(
    store: &RunStore,
    record: &RunRecord,
    from_run_id: &str,
) -> Result<(), StoreError> {
    let features = fix_features(store, from_run_id)?;
    if features.is_empty() {
        return Ok(());
    }
    append_added(
        store,
        record,
        serde_json::json!({ "session": "carried", "from": from_run_id, "features": features }),
    )
}
