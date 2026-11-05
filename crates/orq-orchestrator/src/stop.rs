//! Stopping a live run: end the process that owns it and record the run as failed, so it can be
//! resumed like any other failed run.

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore};

/// End the owner process of `run_id` and append `run.failed`. Only a running (or pause-requested)
/// run can be stopped. The process is signalled only while it is still the recorded owner, so a
/// reused pid is never hit; `terminate` does the signalling.
pub fn stop_run(
    store: &RunStore,
    run_id: &str,
    alive: &dyn Fn(u32, Option<u64>) -> bool,
    terminate: &dyn Fn(u32),
) -> Result<(), String> {
    let record = store.load(run_id).map_err(|e| e.to_string())?;
    let events = store.events(run_id).map_err(|e| e.to_string())?;
    let phase = summarize_run(&events).phase;
    if !matches!(phase, RunPhase::Running | RunPhase::PauseRequested) {
        return Err(format!(
            "{run_id} is {}; only a running run can be stopped",
            phase.as_str()
        ));
    }
    let owner = events.iter().rev().find(|e| e.kind == "run.launched").and_then(|launch| {
        let pid = launch.payload.get("pid")?.as_u64()? as u32;
        Some((pid, launch.payload.get("start_ticks").and_then(|t| t.as_u64())))
    });
    if let Some((pid, start)) = owner {
        if alive(pid, start) {
            terminate(pid);
        }
    }
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(run_id),
            run_id: record.run_id,
            project_id: record.project_id,
            feature_id: record.feature_id,
            kind: "run.failed".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload: serde_json::json!({ "reason": "stopped by the user" }),
        })
        .map_err(|e| e.to_string())
}

/// Send SIGTERM to `pid`.
#[cfg(unix)]
pub fn terminate_process(pid: u32) {
    // SAFETY: kill(2) with a pid and a signal number has no memory-safety preconditions.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
pub fn terminate_process(_pid: u32) {}
