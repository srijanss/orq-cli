//! The verify stage: run the chosen gate steps in the run's worktree, one
//! after the other, stopping at the first one that fails. The push only
//! happens after a pass.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::gate::Gate;
use crate::implement::{next_event_id, now_rfc3339, open_log};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "verify";
/// What a tool needs to be found; the rest of the caller's environment
/// (tokens, CI variables) is not passed on.
const REQUIRED_ENV: [&str; 3] = ["PATH", "HOME", "TERM"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Passed,
    /// `step` is the zero-based index; `exit_code` is `None` when the step
    /// could not be started or was killed by a signal.
    Failed { step: usize, exit_code: Option<i32> },
    Skipped { reason: String },
}

pub fn run_gate_stage(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
) -> Result<GateOutcome, StoreError> {
    run_gate_attempt(store, run_id, gate, enabled, log_path, 1)
}

/// One run of the gate, its events numbered `attempt`.
pub(crate) fn run_gate_attempt(
    store: &RunStore,
    run_id: &str,
    gate: &Gate,
    enabled: bool,
    log_path: &Path,
    attempt: u32,
) -> Result<GateOutcome, StoreError> {
    let record = store.load(run_id)?;
    let skip_reason = match gate {
        _ if !enabled => Some("the local gate is disabled".to_string()),
        Gate::Skipped { reason } => Some(reason.clone()),
        Gate::Steps { .. } => None,
    };
    if let Some(reason) = skip_reason {
        record_event(store, &record, "stage.skipped", attempt, serde_json::json!({ "reason": reason }))?;
        return Ok(GateOutcome::Skipped { reason });
    }
    let Gate::Steps { steps, .. } = gate else {
        return Ok(GateOutcome::Passed);
    };

    let mut log = open_log(log_path)?;
    let log_name = log_path.to_string_lossy().into_owned();
    record_event(store, &record, "stage.started", attempt, serde_json::json!({ "log": log_name, "steps": steps.len() }))?;

    for (index, argv) in steps.iter().enumerate() {
        writeln!(log, "$ {}", argv.join(" "))?;
        let (exit_code, failure) = run_step(&record, argv, &log)?;
        if let Some(failure) = failure {
            let reason = format!("gate step {} ({}) {failure}", index + 1, argv.first().map_or("", String::as_str));
            record_event(
                store,
                &record,
                "stage.failed",
                attempt,
                serde_json::json!({ "exit_code": exit_code, "reason": reason, "log": log_name }),
            )?;
            return Ok(GateOutcome::Failed { step: index, exit_code });
        }
    }
    record_event(store, &record, "stage.completed", attempt, serde_json::json!({ "exit_code": 0, "log": log_name }))?;
    Ok(GateOutcome::Passed)
}

/// Run one step; the second value is why it failed, if it did.
fn run_step(record: &RunRecord, argv: &[String], log: &fs::File) -> Result<(Option<i32>, Option<String>), StoreError> {
    let Some((program, args)) = argv.split_first() else {
        return Ok((None, Some("is empty".to_string())));
    };
    let mut command = Command::new(program);
    command.args(args).current_dir(&record.worktree).env_clear();
    for name in REQUIRED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log.try_clone()?);
    Ok(match command.status() {
        Ok(status) if status.success() => (status.code(), None),
        Ok(status) => match status.code() {
            Some(code) => (Some(code), Some(format!("exited with status {code}"))),
            None => (None, Some("was stopped by a signal".to_string())),
        },
        Err(err) => (None, Some(format!("could not be started: {err}"))),
    })
}

pub(crate) fn record_event(
    store: &RunStore,
    record: &RunRecord,
    kind: &str,
    attempt: u32,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(STAGE.to_string()),
        attempt,
        occurred_at: now_rfc3339(),
        payload,
    })
}
