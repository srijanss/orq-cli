//! Where a run's Herdr pane is. Navigation only: nothing about the run depends on it, and a
//! reader that does not know the event ignores it.

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

const PANE_EVENT: &str = "herdr.pane";

/// The Herdr workspace, tab and pane a run's log is shown in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRef {
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
}

/// Record `herdr.pane` for `run_id`. A later record replaces an earlier one.
pub fn record_pane(store: &RunStore, run_id: &str, pane: &PaneRef) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id,
        project_id: record.project_id,
        feature_id: record.feature_id,
        kind: PANE_EVENT.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({
            "workspace_id": pane.workspace_id,
            "tab_id": pane.tab_id,
            "pane_id": pane.pane_id,
        }),
    })
}

/// The pane most recently recorded for `run_id`, if any.
pub fn run_pane(store: &RunStore, run_id: &str) -> Result<Option<PaneRef>, StoreError> {
    let events = store.events(run_id)?;
    Ok(events
        .iter()
        .rev()
        .filter(|event| event.kind == PANE_EVENT)
        .find_map(|event| {
            let field = |name: &str| {
                event
                    .payload
                    .get(name)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            Some(PaneRef {
                workspace_id: field("workspace_id")?,
                tab_id: field("tab_id")?,
                pane_id: field("pane_id")?,
            })
        }))
}
