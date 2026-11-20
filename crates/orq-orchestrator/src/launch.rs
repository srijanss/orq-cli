//! The event that says a run was started detached from its launcher.

use std::path::Path;

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

/// Record `run.launched` for `run_id`: the runner's process id, its start time and the log its output goes to.
pub fn record_launch(
    store: &RunStore,
    run_id: &str,
    pid: u32,
    log: &Path,
) -> Result<(), StoreError> {
    record(store, run_id, pid, Some(log))
}

/// Record `run.launched` for a run that owns its terminal: the owner's process id and start time,
/// and no log, because its output goes to the launcher's own streams.
pub fn record_owner(store: &RunStore, run_id: &str, pid: u32) -> Result<(), StoreError> {
    record(store, run_id, pid, None)
}

fn record(store: &RunStore, run_id: &str, pid: u32, log: Option<&Path>) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    let mut payload = serde_json::json!({
        "pid": pid,
        // The newest orq.db schema this Orq understands, so a later Orq can tell whether upgrading leaves it behind.
        "schema": orq_core::store::Store::LATEST_VERSION,
        // Tells this process from a later one that reuses the pid; absent where there is no procfs.
        "start_ticks": crate::interrupted::process_start_ticks_in(Path::new("/proc"), pid),
    });
    if let Some(log) = log {
        payload["log"] = serde_json::Value::String(log.to_string_lossy().into_owned());
    }
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: "run.launched".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}

/// Record on each of `run_ids` that orq.db was upgraded from schema `from` to `to` while it ran under an Orq that
/// cannot use the upgraded database, so its failures to record reviews read as what they are.
pub fn record_upgraded_under_runs(
    store: &RunStore,
    run_ids: &[String],
    from: u32,
    to: u32,
) -> Result<(), StoreError> {
    for run_id in run_ids {
        let record = store.load(run_id)?;
        store.append_event(&Event {
            schema_version: 1,
            event_id: next_event_id(run_id),
            run_id: record.run_id,
            project_id: record.project_id,
            feature_id: record.feature_id,
            kind: "store.upgraded_under_runs".to_string(),
            stage: None,
            attempt: 1,
            occurred_at: now_rfc3339(),
            payload: serde_json::json!({"run_ids": run_ids, "from": from, "to": to}),
        })?;
    }
    Ok(())
}
