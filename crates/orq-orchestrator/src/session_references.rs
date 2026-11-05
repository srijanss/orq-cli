//! The sessions a run's agents already ran, kept queryable across a pause and resume (and a model
//! switch). A resumed run is a new run, so the earlier runs' sessions are copied into it as one
//! `run.sessions_carried` event; nothing of the earlier runs' history is changed.

use serde::{Deserialize, Serialize};

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunRecord, RunStore, StoreError};

/// Stages that run an agent session; each is run by the implementer's profile.
const SESSION_STAGES: [&str; 3] = ["implement_and_review", "gate_fix", "ci_fix"];

/// One session that was run: where and on what model, and the harness's id for it when one was known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReference {
    pub run_id: String,
    pub stage: String,
    pub attempt: u32,
    pub role: String,
    pub harness: String,
    pub model: String,
    pub session_id: Option<String>,
}

/// Every session known to `run_id`: the ones carried in from earlier runs, then its own. A carried
/// record that cannot be decoded is an error, never a silently shorter history.
pub fn session_references(
    store: &RunStore,
    run_id: &str,
) -> Result<Vec<SessionReference>, StoreError> {
    let record = store.load(run_id)?;
    let events = store.events(run_id)?;
    let mut all = Vec::new();
    for event in events.iter().filter(|e| e.kind == "run.sessions_carried") {
        let sessions = event
            .payload
            .get("sessions")
            .ok_or_else(|| StoreError::Corrupt(format!("{} has no sessions", event.event_id)))?;
        let carried: Vec<SessionReference> = serde_json::from_value(sessions.clone())
            .map_err(|err| StoreError::Corrupt(format!("{} sessions: {err}", event.event_id)))?;
        all.extend(carried);
    }
    // The sessions of the run itself: one per started session stage, on the profile the run was created with.
    if let Some(profile) = record.profiles.get("implementer") {
        all.extend(
            events
                .iter()
                .filter(|e| e.kind == "stage.started")
                .filter_map(|e| {
                    let stage = e
                        .stage
                        .clone()
                        .filter(|s| SESSION_STAGES.contains(&s.as_str()))?;
                    Some(SessionReference {
                        run_id: run_id.to_string(),
                        stage,
                        attempt: e.attempt,
                        role: "implementer".to_string(),
                        harness: profile.harness.clone(),
                        model: profile.model.clone(),
                        session_id: e
                            .payload
                            .get("session_id")
                            .and_then(|s| s.as_str())
                            .map(str::to_string),
                    })
                }),
        );
    }
    Ok(all)
}

/// The session id a `harness` may continue: the latest one that same harness recorded. A session id
/// belongs to the harness that issued it, so another harness (after a model switch) gets none and
/// starts a fresh session from the handoff instead.
pub fn resumable_session(
    store: &RunStore,
    run_id: &str,
    harness: &str,
) -> Result<Option<String>, StoreError> {
    Ok(session_references(store, run_id)?
        .into_iter()
        .rev()
        .filter(|r| r.harness == harness)
        .find_map(|r| r.session_id))
}

/// Copy the sessions `from_run_id` knows into `record`'s run as a `run.sessions_carried` event.
/// Only appends to the new run; the earlier run's records are left as they are.
pub fn carry_session_references(
    store: &RunStore,
    record: &RunRecord,
    from_run_id: &str,
) -> Result<(), StoreError> {
    let sessions = session_references(store, from_run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "run.sessions_carried".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "from": from_run_id, "sessions": sessions }),
    })
}
