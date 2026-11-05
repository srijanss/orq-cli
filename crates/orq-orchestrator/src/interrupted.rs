//! A run whose owning process is gone: its events stop mid-flight, so it would read as running
//! forever. Reconciling records that it was interrupted, which makes it resumable like a failed run.

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore, StoreError};

/// If `run_id` still reads as running but the process that owns it (per its latest
/// `run.launched`) is gone according to `alive`, append `run.failed`. Returns whether it did.
pub fn reconcile_interrupted(store: &RunStore, run_id: &str, alive: &dyn Fn(u32) -> bool) -> Result<bool, StoreError> {
    let events = store.events(run_id)?;
    if summarize_run(&events).phase != RunPhase::Running {
        return Ok(false);
    }
    let Some(pid) = events.iter().rev().find(|e| e.kind == "run.launched").and_then(|e| e.payload.get("pid")).and_then(|p| p.as_u64()) else {
        return Ok(false);
    };
    let pid = pid as u32;
    if alive(pid) {
        return Ok(false);
    }
    let record = store.load(run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: "run.failed".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "reason": format!("interrupted: its process {pid} is gone") }),
    })?;
    Ok(true)
}
