//! The event that says a run was started detached from its launcher.

use std::path::Path;

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

/// Record `run.launched` for `run_id`: the runner's process id and the log its output goes to.
pub fn record_launch(store: &RunStore, run_id: &str, pid: u32, log: &Path) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: "run.launched".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "pid": pid, "log": log.to_string_lossy() }),
    })
}
