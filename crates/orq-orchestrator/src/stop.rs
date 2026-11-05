//! Stopping a live run: end the process that owns it and record the run as failed, so it can be
//! resumed like any other failed run.

use std::time::Duration;

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS_PER_SIGNAL: u32 = 30;

/// End the owner process of `run_id` and append `run.failed`. Only a running (or pause-requested)
/// run can be stopped. The owner is signalled only when its recorded pid is a single real process
/// and its recorded start time identifies it, so a reused pid is never hit. `signal(pid, force)`
/// sends SIGTERM (or SIGKILL when `force`); `sleep` paces the wait for the owner to exit. The run
/// is recorded failed only once the owner is confirmed gone.
pub fn stop_run(
    store: &RunStore,
    run_id: &str,
    alive: &dyn Fn(u32, Option<u64>) -> bool,
    signal: &dyn Fn(u32, bool) -> Result<(), String>,
    sleep: &dyn Fn(Duration),
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
    if let Some(launch) = events.iter().rev().find(|e| e.kind == "run.launched") {
        let pid = launch
            .payload
            .get("pid")
            .and_then(|p| p.as_u64())
            .and_then(|p| i32::try_from(p).ok())
            .filter(|p| *p > 1)
            .ok_or_else(|| format!("{run_id} records an owner pid that cannot be signalled safely"))?
            as u32;
        let start = launch
            .payload
            .get("start_ticks")
            .and_then(|t| t.as_u64())
            .ok_or_else(|| format!("cannot verify the owner of {run_id}; refusing to signal pid {pid}"))?;
        if alive(pid, Some(start)) {
            signal(pid, false)?;
            if !exited(pid, start, alive, sleep) {
                signal(pid, true)?;
                if !exited(pid, start, alive, sleep) {
                    return Err(format!("the owner of {run_id} (pid {pid}) is still running"));
                }
            }
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

fn exited(
    pid: u32,
    start: u64,
    alive: &dyn Fn(u32, Option<u64>) -> bool,
    sleep: &dyn Fn(Duration),
) -> bool {
    for _ in 0..POLLS_PER_SIGNAL {
        if !alive(pid, Some(start)) {
            return true;
        }
        sleep(POLL_INTERVAL);
    }
    !alive(pid, Some(start))
}

/// Send SIGTERM (or SIGKILL when `force`) to the single process `pid`.
#[cfg(unix)]
pub fn terminate_process(pid: u32, force: bool) -> Result<(), String> {
    let pid = i32::try_from(pid).ok().filter(|p| *p > 1).ok_or_else(|| format!("refusing to signal pid {pid}"))?;
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill(2) with a validated positive pid and a signal number has no memory-safety preconditions.
    if unsafe { libc::kill(pid as libc::pid_t, sig) } == 0 {
        Ok(())
    } else {
        Err(format!("cannot signal pid {pid}: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(not(unix))]
pub fn terminate_process(_pid: u32, _force: bool) -> Result<(), String> {
    Err("stopping a run is not supported on this platform".to_string())
}
