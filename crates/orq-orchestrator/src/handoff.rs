//! The record a model switch leaves for whoever continues the work: `handoff.json` in the resumed
//! run's directory. It holds state (the feature plan, the diff against the base, the latest gate
//! result, the branch and the worktree), never a conversation: no transcript and no session ids, so
//! it can seed a session on any harness.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::checkpoint::latest_checkpoint;
use crate::store::{RunRecord, RunStore, StoreError};

pub const HANDOFF_FILE: &str = "handoff.json";

/// The gate stage's name in the event log.
const GATE_STAGE: &str = "verify";

/// One feature of the plan and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub description: Option<String>,
    pub status: String,
}

/// One file changed against the base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// The diff of the branch against its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffDiff {
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<DiffFile>,
}

/// How the latest gate run ended: `passed`, `failed`, `skipped`, or `not_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub outcome: String,
    pub reason: Option<String>,
    pub exit_code: Option<i64>,
    pub log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub schema_version: u32,
    pub from_run: String,
    pub run_id: String,
    pub feature_id: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub plan: Vec<PlanItem>,
    pub diff: HandoffDiff,
    pub gate: GateResult,
    pub completed_stages: Vec<String>,
    pub next_stage: Option<String>,
}

/// The latest gate result among `run_id`'s events.
fn latest_gate(store: &RunStore, run_id: &str) -> Result<GateResult, StoreError> {
    let events = store.events(run_id)?;
    let latest = events.iter().rev().find(|e| {
        e.stage.as_deref() == Some(GATE_STAGE) && matches!(e.kind.as_str(), "stage.completed" | "stage.failed" | "stage.skipped")
    });
    let text = |e: &crate::store::Event, key: &str| e.payload.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Ok(match latest {
        None => GateResult { outcome: "not_run".to_string(), reason: None, exit_code: None, log: None },
        Some(e) => GateResult {
            outcome: match e.kind.as_str() {
                "stage.completed" => "passed",
                "stage.failed" => "failed",
                _ => "skipped",
            }
            .to_string(),
            reason: text(e, "reason"),
            exit_code: e.payload.get("exit_code").and_then(|v| v.as_i64()),
            log: text(e, "log"),
        },
    })
}

/// Write `handoff.json` into `record`'s run directory, describing the state `from_run_id` stopped in.
/// The plan and the diff come from the caller, which owns the ledger and the repository.
pub fn write_handoff(
    store: &RunStore,
    record: &RunRecord,
    from_run_id: &str,
    plan: Vec<PlanItem>,
    diff: HandoffDiff,
) -> Result<PathBuf, StoreError> {
    let checkpoint = latest_checkpoint(store, from_run_id)?;
    let handoff = Handoff {
        schema_version: 1,
        from_run: from_run_id.to_string(),
        run_id: record.run_id.clone(),
        feature_id: record.feature_id.clone(),
        branch: record.branch.clone(),
        worktree: record.worktree.clone(),
        plan,
        diff,
        gate: latest_gate(store, from_run_id)?,
        completed_stages: checkpoint.as_ref().map(|c| c.completed_stages.clone()).unwrap_or_default(),
        next_stage: checkpoint.and_then(|c| c.next_stage),
    };
    let bytes = serde_json::to_vec_pretty(&handoff).map_err(|err| StoreError::Corrupt(err.to_string()))?;
    store.write_run_file(&record.run_id, HANDOFF_FILE, &bytes)
}

/// The handoff written for `run_id`, or none when that run began without a model switch. A file that
/// is present but unreadable or not a handoff is an error: it is never taken for "no handoff".
pub fn read_handoff(store: &RunStore, run_id: &str) -> Result<Option<Handoff>, StoreError> {
    let Some(bytes) = store.read_run_file(run_id, HANDOFF_FILE)? else { return Ok(None) };
    serde_json::from_slice(&bytes).map(Some).map_err(|err| StoreError::Corrupt(format!("{HANDOFF_FILE}: {err}")))
}

/// The prompt text that tells a new implementer session where the work stands. It is built from the
/// record alone, so it serves a session on any harness: it names no session of the previous model and
/// asks nothing of that model's conversation.
pub fn handoff_prompt(handoff: &Handoff) -> String {
    let mut out = String::from(
        "You are continuing work another model started. You have no access to the previous session's conversation; \
         the state below, and the repository itself, are all you have.\n",
    );
    out.push_str(&format!("\nFeature: {}\nBranch: {}\nCheckout: {}\n", handoff.feature_id, handoff.branch, handoff.worktree.display()));
    out.push_str("\nPlan:\n");
    for item in &handoff.plan {
        match &item.description {
            Some(description) => out.push_str(&format!("- {} ({}): {}\n", item.id, item.status, description)),
            None => out.push_str(&format!("- {} ({})\n", item.id, item.status)),
        }
    }
    out.push_str(&format!("\nChanged so far ({}..{}):\n", handoff.diff.base_sha, handoff.diff.head_sha));
    for file in &handoff.diff.files {
        match (file.added, file.deleted) {
            (Some(added), Some(deleted)) => out.push_str(&format!("- {} (+{} -{})\n", file.path, added, deleted)),
            _ => out.push_str(&format!("- {}\n", file.path)),
        }
    }
    out.push_str(&format!("\nLatest test run: {}", handoff.gate.outcome));
    if let Some(reason) = &handoff.gate.reason {
        out.push_str(&format!(" ({reason})"));
    }
    if let Some(log) = &handoff.gate.log {
        out.push_str(&format!("; log: {log}"));
    }
    out.push('\n');
    out.push_str(&format!("\nCompleted stages: {}\n", handoff.completed_stages.join(", ")));
    if let Some(next) = &handoff.next_stage {
        out.push_str(&format!("Remaining: {next}\n"));
    }
    out
}
