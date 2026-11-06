//! An attended session waiting for its person: what Claude's hook input says is needed, and the
//! `run.needs_input` event that puts the need on the run for anything showing it.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::implement::{next_event_id, now_rfc3339};
use crate::resume::members_of;
use crate::retention_facts::parse_rfc3339;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const EVENT: &str = "run.needs_input";
const ANSWERED_EVENT: &str = "run.input_given";
const QUESTION: &str = "question";
const PERMISSION: &str = "permission";
const MAX_TEXT_CHARS: usize = 1000;
/// A question also fires a permission prompt, some 5 seconds after it. Nothing in that prompt tells it
/// from another tool's, and a question rejected or interrupted leaves no answer event: only the time
/// between them does. Kept short, since a second notification costs less than a permission untold.
const SAME_NEED_SECS: u64 = 10;

/// What a session is waiting for its person to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsInput {
    kind: &'static str,
    text: String,
}

impl NeedsInput {
    /// What is asked, redacted and on one line.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How a notification about this need ends its first words.
    pub fn ending(&self) -> &'static str {
        if self.kind == QUESTION {
            "needs your answer"
        } else {
            "needs your permission"
        }
    }
}

/// The need in the JSON a Claude hook is given on stdin: a question it is about to ask, or a
/// permission it is waiting for. Anything else is no need.
pub fn parse_needs_input(input: &str) -> Option<NeedsInput> {
    let input: serde_json::Value = serde_json::from_str(input).ok()?;
    let field = |name: &str| input.get(name).and_then(|value| value.as_str());
    match field("hook_event_name")? {
        "PreToolUse" if field("tool_name")? == "AskUserQuestion" => {
            let questions = input.get("tool_input")?.get("questions")?.as_array()?;
            let asked: Vec<&str> = questions
                .iter()
                .filter_map(|question| question.get("question")?.as_str())
                .collect();
            (!asked.is_empty()).then(|| NeedsInput { kind: QUESTION, text: one_line(&asked.join(" ")) })
        }
        "Notification" if field("notification_type")? == "permission_prompt" => {
            Some(NeedsInput { kind: PERMISSION, text: one_line(field("message").unwrap_or_default()) })
        }
        _ => None,
    }
}

/// Whether the JSON a Claude hook is given on stdin says a question has just been answered.
pub fn question_was_answered(input: &str) -> bool {
    let Ok(input) = serde_json::from_str::<serde_json::Value>(input) else { return false };
    input["hook_event_name"] == "PostToolUse" && input["tool_name"] == "AskUserQuestion"
}

fn one_line(text: &str) -> String {
    orq_core::redact::redact(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT_CHARS)
        .collect()
}

/// Puts `need` on the run and returns the run's features to name when telling its person. `None`
/// when it is the permission prompt of a question just recorded, which is already told about.
pub fn record_needs_input(store: &RunStore, run_id: &str, need: &NeedsInput) -> Result<Option<String>, StoreError> {
    let record = store.load(run_id)?;
    let events = store.events(run_id)?;
    if need.kind == PERMISSION && follows_a_question(&events) {
        return Ok(None);
    }
    append(store, &record, EVENT, serde_json::json!({"kind": need.kind, "text": need.text}))?;
    Ok(Some(members_of(&record, &events).join(",")))
}

/// Puts on the run that its person has answered. What they answered stays out of the record.
pub fn record_input_given(store: &RunStore, run_id: &str) -> Result<(), StoreError> {
    let record = store.load(run_id)?;
    append(store, &record, ANSWERED_EVENT, serde_json::json!({}))
}

fn append(store: &RunStore, record: &RunRecord, kind: &str, payload: serde_json::Value) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: None,
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}

fn follows_a_question(events: &[Event]) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let latest = events.iter().rev().find(|e| e.kind == ANSWERED_EVENT || (e.kind == EVENT && e.payload["kind"] == QUESTION));
    latest
        .filter(|event| event.kind == EVENT)
        .and_then(|event| parse_rfc3339(&event.occurred_at))
        .is_some_and(|asked| now.saturating_sub(asked) < SAME_NEED_SECS)
}
