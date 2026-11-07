//! When the gate fails: let a harness session fix it and rerun the gate, up to
//! a number of attempts. A gate still red after that fails the run; the
//! worktree is left as it is for inspection.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::gate::Gate;
use crate::gate_stage::{record_event, run_gate_attempt, GateOutcome};
use crate::implement::{run_session_stage, ImplementSession};
use crate::store::{RunStore, StoreError};

const FIX_STAGE: &str = "gate_fix";

/// An owned description of one fix session, built per attempt by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixSessionSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub log: PathBuf,
    pub terminal: bool,
    /// A file the session leaves when it is finished, at which orq closes it; `None` waits for it to exit.
    pub exit_marker: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateFixOutcome {
    /// The gate passed, after `fix_attempts` fix sessions.
    Passed {
        fix_attempts: u32,
    },
    Skipped {
        reason: String,
    },
    /// Still red after `fix_attempts` fix sessions; the run has failed.
    Failed {
        fix_attempts: u32,
        step: usize,
        exit_code: Option<i32>,
    },
    /// The fix session reported that the failure is not caused by this run's changes and left it
    /// alone; `summary` is what it wrote, for the person to decide on.
    Unrelated { summary: String },
}

/// The file, next to the gate log, in which a fix session says the failure is not this run's.
pub const UNRELATED_FAILURE_FILE: &str = "unrelated-failure.txt";

/// What the fix session wrote in the report, if it is a plain file with something in it. A link
/// (the session could point it at any file orq can read) or anything else is not read.
pub(crate) fn read_report(report: &Path) -> Option<String> {
    // Opened without following a link and without waiting on a pipe, then checked on the opened
    // handle, so the path cannot be swapped between the check and the read.
    // Where no such open exists, no report is read at all rather than one that follows links.
    #[cfg(not(unix))]
    {
        let _ = report;
        return None;
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(report)
            .ok()?
    };
    #[cfg(unix)]
    read_report_from(file)
}

#[cfg(unix)]
fn read_report_from(file: std::fs::File) -> Option<String> {
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    // Only the start of the file is read, and only its first line is kept, so a session cannot
    // make orq hold or print an unbounded amount of text.
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(MAX_SUMMARY_CHARS).collect())
}

/// Remove whatever is in the report's place, so a session starts without one. A directory the
/// session left there is removed too (links inside it are removed, never followed).
pub(crate) fn clear_report(report: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(report) {
        Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(report),
        Ok(_) => std::fs::remove_file(report),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

const MAX_REPORT_BYTES: u64 = 4096;
const MAX_SUMMARY_CHARS: usize = 500;

/// Run the gate; while it fails and fewer than `max_attempts` fixes were tried,
/// ask `fixer` for the session of attempt `n` (counting from 1, given the gate
/// log so the session can be told what failed) and rerun the gate.
pub fn run_gate_with_fixes(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    gate_log: &Path,
    max_attempts: u32,
    fixer: impl FnMut(u32, &Path) -> FixSessionSpec,
) -> Result<GateFixOutcome, StoreError> {
    run_gate_with_fixes_env(
        store,
        run_id,
        gate,
        enabled,
        gate_log,
        max_attempts,
        &[],
        fixer,
    )
}

/// Like [`run_gate_with_fixes`], with `env` set for every run of the gate.
#[allow(clippy::too_many_arguments)]
pub fn run_gate_with_fixes_env(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    gate_log: &Path,
    max_attempts: u32,
    env: &[(String, String)],
    mut fixer: impl FnMut(u32, &Path) -> FixSessionSpec,
) -> Result<GateFixOutcome, StoreError> {
    let mut fix_attempts = 0;
    loop {
        match run_gate_attempt(
            store,
            run_id,
            gate,
            enabled,
            gate_log,
            fix_attempts + 1,
            env,
        )? {
            GateOutcome::Passed => return Ok(GateFixOutcome::Passed { fix_attempts }),
            GateOutcome::Skipped { reason } => return Ok(GateFixOutcome::Skipped { reason }),
            GateOutcome::Failed { step, exit_code } => {
                if fix_attempts >= max_attempts {
                    let record = store.load(run_id)?;
                    let reason =
                        format!("the gate is still failing after {fix_attempts} fix attempt(s)");
                    record_event(
                        store,
                        &record,
                        "run.failed",
                        fix_attempts + 1,
                        serde_json::json!({ "reason": reason, "log": gate_log.to_string_lossy() }),
                    )?;
                    return Ok(GateFixOutcome::Failed {
                        fix_attempts,
                        step,
                        exit_code,
                    });
                }
                fix_attempts += 1;
                // A report left by an earlier session or run must not speak for this one.
                let report = gate_log.with_file_name(UNRELATED_FAILURE_FILE);
                if let Err(e) = clear_report(&report) {
                    return Err(StoreError::Corrupt(format!(
                        "cannot clear the earlier unrelated-failure report {}: {e}",
                        report.display()
                    )));
                }
                let spec = fixer(fix_attempts, gate_log);
                let session = ImplementSession {
                    program: &spec.program,
                    args: &spec.args,
                    env: &spec.env,
                    cwd: &spec.cwd,
                    session_id: spec.session_id.as_deref(),
                    log: &spec.log,
                    terminal: spec.terminal,
                    exit_marker: spec.exit_marker.as_deref(),
                };
                // The session's own result is not trusted: the rerun gate decides.
                run_session_stage(store, run_id, &session, FIX_STAGE, fix_attempts)?;
                if let Some(summary) = read_report(&report) {
                    return Ok(GateFixOutcome::Unrelated { summary });
                }
            }
        }
    }
}
