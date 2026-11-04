//! When the gate fails: let a harness session fix it and rerun the gate, up to
//! a number of attempts. A gate still red after that fails the run; the
//! worktree is left as it is for inspection.

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateFixOutcome {
    /// The gate passed, after `fix_attempts` fix sessions.
    Passed { fix_attempts: u32 },
    Skipped { reason: String },
    /// Still red after `fix_attempts` fix sessions; the run has failed.
    Failed { fix_attempts: u32, step: usize, exit_code: Option<i32> },
}

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
    mut fixer: impl FnMut(u32, &Path) -> FixSessionSpec,
) -> Result<GateFixOutcome, StoreError> {
    let mut fix_attempts = 0;
    loop {
        match run_gate_attempt(store, run_id, gate, enabled, gate_log, fix_attempts + 1, &[])? {
            GateOutcome::Passed => return Ok(GateFixOutcome::Passed { fix_attempts }),
            GateOutcome::Skipped { reason } => return Ok(GateFixOutcome::Skipped { reason }),
            GateOutcome::Failed { step, exit_code } => {
                if fix_attempts >= max_attempts {
                    let record = store.load(run_id)?;
                    let reason = format!("the gate is still failing after {fix_attempts} fix attempt(s)");
                    record_event(store, &record, "run.failed", fix_attempts + 1, serde_json::json!({ "reason": reason, "log": gate_log.to_string_lossy() }))?;
                    return Ok(GateFixOutcome::Failed { fix_attempts, step, exit_code });
                }
                fix_attempts += 1;
                let spec = fixer(fix_attempts, gate_log);
                let session = ImplementSession {
                    program: &spec.program,
                    args: &spec.args,
                    env: &spec.env,
                    cwd: &spec.cwd,
                    session_id: spec.session_id.as_deref(),
                    log: &spec.log,
                    terminal: spec.terminal,
                };
                // The session's own result is not trusted: the rerun gate decides.
                run_session_stage(store, run_id, &session, FIX_STAGE, fix_attempts)?;
            }
        }
    }
}
