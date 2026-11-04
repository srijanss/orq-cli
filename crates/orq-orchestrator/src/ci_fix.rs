//! When CI is red on the pushed branch: let a harness session fix it, gate and push the
//! fix, and wait on CI again, up to a number of attempts. A CI still red after that fails
//! the run; the worktree is left as it is for inspection.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ci_wait::CiOutcome;
use crate::gate::Gate;
use crate::gate_fix::FixSessionSpec;
use crate::gate_stage::{run_gate_attempt, GateOutcome};
use crate::implement::{next_event_id, now_rfc3339, run_session_stage, ImplementSession};
use crate::push_stage::PushOutcome;
use crate::store::{Event, RunStore, StoreError};

const FIX_STAGE: &str = "ci_fix";

/// What the loop needs from the outside world. The orchestrator owns the order of events;
/// the caller owns talking to the CI provider, pushing, and describing failures.
pub trait CiFixSteps {
    /// Wait for CI on the commit `head_sha`.
    fn wait(&mut self, head_sha: &str) -> Result<CiOutcome, StoreError>;
    /// Publish the branch again after a fix.
    fn push(&mut self) -> Result<PushOutcome, StoreError>;
    /// The text of the failure log for these failing checks (fetched from the provider).
    fn failure_log(&mut self, failing: &[String]) -> String;
    /// The fix session of attempt `attempt` (from 1), given the file holding the failure log.
    fn fix_session(&mut self, attempt: u32, failure_log: &Path) -> FixSessionSpec;
}

pub struct CiFixConfig<'a> {
    pub gate: &'a Gate,
    pub gate_enabled: bool,
    pub gate_log: &'a Path,
    /// Where each attempt's failure log is written (created if missing).
    pub log_dir: &'a Path,
    pub max_attempts: u32,
    /// Set for every gate run after a fix.
    pub env: &'a [(String, String)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiFixOutcome {
    /// CI is green, after `fix_attempts` fix sessions.
    Passed { fix_attempts: u32 },
    /// CI is disabled for the run.
    Skipped,
    /// CI never gave a verdict in time; nothing was fixed.
    TimedOut { fix_attempts: u32 },
    /// CI could not be read.
    Errored { fix_attempts: u32, reason: String },
    /// Still red after `fix_attempts` fix sessions; the run has failed.
    Failed { fix_attempts: u32, failing: Vec<String> },
    /// A fix left the gate red, so it was not pushed; the run has failed.
    GateFailed { fix_attempts: u32, step: usize, exit_code: Option<i32> },
    /// A fix could not be pushed; the run has failed.
    PushFailed { fix_attempts: u32, reason: String },
}

/// Wait on CI for `head_sha`; while it is red and fewer than `config.max_attempts` fixes
/// were tried, write the failure log, run a fix session, rerun the gate, push, and wait on
/// the pushed commit.
pub fn run_ci_fix_loop(
    store: &RunStore,
    run_id: &str,
    head_sha: &str,
    config: &CiFixConfig,
    steps: &mut impl CiFixSteps,
) -> Result<CiFixOutcome, StoreError> {
    let worktree = store.load(run_id)?.worktree;
    let mut head = head_sha.to_string();
    let mut fix_attempts = 0;
    loop {
        let failing = match steps.wait(&head)? {
            CiOutcome::Passed => return Ok(CiFixOutcome::Passed { fix_attempts }),
            CiOutcome::Skipped => return Ok(CiFixOutcome::Skipped),
            CiOutcome::TimedOut => return Ok(CiFixOutcome::TimedOut { fix_attempts }),
            CiOutcome::Errored { reason } => return Ok(CiFixOutcome::Errored { fix_attempts, reason }),
            CiOutcome::Failed { failing } => failing,
        };
        if fix_attempts >= config.max_attempts {
            let reason = format!("CI is still failing after {fix_attempts} fix attempt(s)");
            fail_run(store, run_id, fix_attempts + 1, serde_json::json!({ "reason": reason, "failing": failing }))?;
            return Ok(CiFixOutcome::Failed { fix_attempts, failing });
        }
        fix_attempts += 1;

        let failure_log = config.log_dir.join(format!("ci-failure-{fix_attempts}.log"));
        let failure_log = write_failure_log(&worktree, config.log_dir, &failure_log, &steps.failure_log(&failing))?;

        let spec = steps.fix_session(fix_attempts, &failure_log);
        let session = ImplementSession {
            program: &spec.program,
            args: &spec.args,
            env: &spec.env,
            cwd: &spec.cwd,
            session_id: spec.session_id.as_deref(),
            log: &spec.log,
            terminal: spec.terminal,
        };
        // The session's own result is not trusted: the gate and CI decide.
        run_session_stage(store, run_id, &session, FIX_STAGE, fix_attempts)?;

        match run_gate_attempt(store, run_id, config.gate, config.gate_enabled, config.gate_log, fix_attempts, config.env)? {
            GateOutcome::Passed | GateOutcome::Skipped { .. } => {}
            GateOutcome::Failed { step, exit_code } => {
                let reason = format!("the gate failed after CI fix attempt {fix_attempts}; nothing was pushed");
                fail_run(store, run_id, fix_attempts, serde_json::json!({ "reason": reason }))?;
                return Ok(CiFixOutcome::GateFailed { fix_attempts, step, exit_code });
            }
        }

        match steps.push()? {
            PushOutcome::Pushed { head_sha } => head = head_sha,
            PushOutcome::Failed { reason } => {
                let message = format!("pushing CI fix attempt {fix_attempts} failed: {reason}");
                fail_run(store, run_id, fix_attempts, serde_json::json!({ "reason": message }))?;
                return Ok(CiFixOutcome::PushFailed { fix_attempts, reason });
            }
        }
    }
}

/// Write one attempt's failure log and return the path it got. The log directory may sit
/// inside the worktree, so whatever the repository or an earlier session left there is not
/// trusted: the file is created exclusively (never through an existing file or symlink; a
/// taken name moves on to the next free one), owner-only, and neither the directory nor
/// any directory below the worktree on the way to it may be a symlink. CI output can carry
/// secrets. A directory swapped for a symlink between the check and the open is not guarded
/// against; a log directory outside the worktree avoids the question.
fn write_failure_log(worktree: &Path, dir: &Path, path: &Path, text: &str) -> Result<PathBuf, StoreError> {
    use std::io::Write;
    reject_symlinked_ancestors(worktree, dir)?;
    match fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the CI failure log directory is a symlink or not a directory").into());
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(dir)?,
        Err(err) => return Err(err.into()),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // A log from an earlier invocation (or another run sharing the directory) is never
    // overwritten nor fatal: take the next free name, `ci-failure-N.log`, `ci-failure-N.1.log`, ...
    let mut candidate = path.to_path_buf();
    for suffix in 1u32.. {
        match options.open(&candidate) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(candidate);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                candidate = path.with_extension(format!("{suffix}.log"));
            }
            Err(err) => return Err(err.into()),
        }
    }
    unreachable!("the suffix counter does not run out")
}

/// Every directory between the worktree and the log directory (the worktree's own path is
/// the caller's, and may sit behind system symlinks) must be a real directory if it exists:
/// a repository-planted `.orq -> /elsewhere` must not carry the log out of the tree.
fn reject_symlinked_ancestors(worktree: &Path, dir: &Path) -> Result<(), StoreError> {
    let Ok(relative) = dir.strip_prefix(worktree) else { return Ok(()) };
    let mut current = worktree.to_path_buf();
    for part in relative.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a directory on the way to the CI failure log directory is a symlink",
                )
                .into());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn fail_run(store: &RunStore, run_id: &str, attempt: u32, payload: serde_json::Value) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    // Recorded under this stage, not the gate's: the run failed here, whatever the gate said.
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "run.failed".to_string(),
        stage: Some(FIX_STAGE.to_string()),
        attempt,
        occurred_at: now_rfc3339(),
        payload,
    })
}
