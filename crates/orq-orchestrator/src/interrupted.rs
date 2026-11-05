//! A run whose owning process is gone: its events stop mid-flight, so it would read as running
//! forever. Reconciling records that it was interrupted, which makes it resumable like a failed run.

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore, StoreError};

/// If `run_id` still reads as running but the process that owns it (per its latest
/// `run.launched`, with the start time it recorded) is gone according to `alive`, append `run.failed`. Returns whether it did.
pub fn reconcile_interrupted(
    store: &RunStore,
    run_id: &str,
    alive: &dyn Fn(u32, Option<u64>) -> bool,
) -> Result<bool, StoreError> {
    let events = store.events(run_id)?;
    // A run that was asked to pause but never reached its checkpoint is as stuck as a running one;
    // a paused run is waiting on purpose and keeps its checkpoint.
    if !matches!(
        summarize_run(&events).phase,
        RunPhase::Running | RunPhase::PauseRequested
    ) {
        return Ok(false);
    }
    let Some(launch) = events.iter().rev().find(|e| e.kind == "run.launched") else {
        return Ok(false);
    };
    let Some(pid) = launch.payload.get("pid").and_then(|p| p.as_u64()) else {
        return Ok(false);
    };
    let pid = pid as u32;
    let recorded_start = launch.payload.get("start_ticks").and_then(|t| t.as_u64());
    if alive(pid, recorded_start) {
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

/// When `pid` started, in clock ticks since boot (field 22 of `<proc_root>/<pid>/stat`), if readable.
pub fn process_start_ticks_in(proc_root: &std::path::Path, pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    // `comm` (field 2) may contain spaces and parentheses, so count from the last ')'.
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// Whether `pid` is still the process that was recorded as owner. A pid whose start time differs
/// from the recorded one was reused by another process. With no recorded start time (an older
/// record) or an unreadable one, only the pid check applies, so a live run is never taken over.
pub fn owner_is_alive_in(
    proc_root: &std::path::Path,
    pid: u32,
    recorded_start: Option<u64>,
) -> bool {
    if !process_is_alive_in(proc_root, pid) {
        return false;
    }
    match (recorded_start, process_start_ticks_in(proc_root, pid)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

/// [`owner_is_alive_in`] over the system's `/proc`.
pub fn process_is_alive(pid: u32, recorded_start: Option<u64>) -> bool {
    owner_is_alive_in(std::path::Path::new("/proc"), pid, recorded_start)
}
