//! A run whose owning process is gone: its events stop mid-flight, so it would read as running
//! forever. Reconciling records that it was interrupted, which makes it resumable like a failed run.

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore, StoreError};

/// If `run_id` still reads as running but the process that owns it (per its latest
/// `run.launched`) is gone according to `alive`, append `run.failed`. Returns whether it did.
pub fn reconcile_interrupted(store: &RunStore, run_id: &str, alive: &dyn Fn(u32) -> bool) -> Result<bool, StoreError> {
    let events = store.events(run_id)?;
    // A run that was asked to pause but never reached its checkpoint is as stuck as a running one;
    // a paused run is waiting on purpose and keeps its checkpoint.
    if !matches!(summarize_run(&events).phase, RunPhase::Running | RunPhase::PauseRequested) {
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

/// Whether the process `pid` is alive, read from a procfs mounted at `proc_root`. Only a procfs that
/// answers for itself (`self` exists) and has no entry for `pid` counts as dead; a missing procfs
/// or an entry that cannot be inspected is "unknown", which reads as alive so a live run is never
/// taken over.
pub fn process_is_alive_in(proc_root: &std::path::Path, pid: u32) -> bool {
    if !proc_root.join("self").exists() {
        return true;
    }
    match std::fs::metadata(proc_root.join(pid.to_string())) {
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
        Ok(_) => true,
    }
}

/// [`process_is_alive_in`] over the system's `/proc`.
pub fn process_is_alive(pid: u32) -> bool {
    process_is_alive_in(std::path::Path::new("/proc"), pid)
}
