//! Stopping a live run: end the process that owns it and record the run as failed, so it can be
//! resumed like any other failed run.

use std::path::Path;
use std::time::Duration;

use crate::implement::{next_event_id, now_rfc3339};
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunStore};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS_PER_SIGNAL: u32 = 30;

/// What is known about the recorded owner of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
    /// The recorded process is executing: same pid, same start time, not a zombie.
    Running,
    /// The recorded process is gone: no such pid, a different process holds it now, or it is an
    /// unreaped zombie.
    Exited,
    /// Its identity cannot be established (no procfs, unreadable or unparsable stat).
    Unknown,
}

/// [`OwnerState`] of `pid` against the recorded `start` ticks, read from `<proc_root>/<pid>/stat`.
/// Unlike a liveness check it fails closed: anything it cannot read is `Unknown`, never alive.
pub fn owner_state_in(proc_root: &Path, pid: u32, start: u64) -> OwnerState {
    if !proc_root.join("self").exists() {
        return OwnerState::Unknown;
    }
    let stat = match std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return OwnerState::Exited,
        Err(_) => return OwnerState::Unknown,
    };
    // `comm` (field 2) may contain spaces and parentheses, so count from the last ')'.
    let Some(after_comm) = stat.rfind(')').map(|i| &stat[i + 1..]) else { return OwnerState::Unknown };
    let mut fields = after_comm.split_whitespace();
    let state = fields.next();
    let Some(ticks) = fields.nth(18).and_then(|t| t.parse::<u64>().ok()) else { return OwnerState::Unknown };
    match state {
        Some("Z" | "X" | "x") => OwnerState::Exited,
        Some(_) if ticks != start => OwnerState::Exited,
        Some(_) => OwnerState::Running,
        None => OwnerState::Unknown,
    }
}

/// [`owner_state_in`] over the system's `/proc`.
pub fn owner_state(pid: u32, start: u64) -> OwnerState {
    owner_state_in(Path::new("/proc"), pid, start)
}

/// End the owner process of `run_id` and append `run.failed`. Only a running (or pause-requested)
/// run can be stopped. The owner is signalled only when its recorded pid is a single real process
/// and its recorded start time identifies it, so a reused pid is never hit. `signal(pid, force)`
/// sends SIGTERM (or SIGKILL when `force`); `sleep` paces the wait for the owner to exit. The run
/// is recorded failed only once the owner is confirmed gone.
pub fn stop_run(
    store: &RunStore,
    run_id: &str,
    probe: &dyn Fn(u32, u64) -> OwnerState,
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
        match probe(pid, start) {
            OwnerState::Exited => {}
            OwnerState::Unknown => {
                return Err(format!("cannot verify the owner of {run_id}; refusing to signal pid {pid}"));
            }
            OwnerState::Running => {
                signal(pid, false)?;
                if wait_for_exit(pid, start, probe, sleep)? == OwnerState::Running {
                    signal(pid, true)?;
                    if wait_for_exit(pid, start, probe, sleep)? == OwnerState::Running {
                        return Err(format!("the owner of {run_id} (pid {pid}) is still running"));
                    }
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

/// Poll until the owner is `Exited`; returns the last state seen. An owner whose identity is
/// lost (`Unknown`) is an error, so the caller never escalates against an unverified pid.
fn wait_for_exit(
    pid: u32,
    start: u64,
    probe: &dyn Fn(u32, u64) -> OwnerState,
    sleep: &dyn Fn(Duration),
) -> Result<OwnerState, String> {
    let mut state = OwnerState::Running;
    for _ in 0..=POLLS_PER_SIGNAL {
        state = probe(pid, start);
        match state {
            OwnerState::Exited => return Ok(state),
            OwnerState::Unknown => {
                return Err(format!("lost track of the owner (pid {pid}) while stopping it; not signalling further"));
            }
            OwnerState::Running => sleep(POLL_INTERVAL),
        }
    }
    Ok(state)
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
