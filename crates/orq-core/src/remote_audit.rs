//! The audit log of remote actions: one JSON line appended for every attempt, allowed or refused, saying
//! when, who (the signed-in passkey's label), what, and on which project and run. An attempt is built
//! only from these named fields, so no header or token can reach the file, and free text is redacted
//! on the way in. Nothing here rewrites or removes a line; the web reads the log and cannot change it.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

use serde_json::{json, Value};

use crate::redact::redact;
use crate::storage::StorageRoots;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allowed,
    Refused,
}

/// One action somebody tried from the web.
pub struct Attempt<'a> {
    pub identity: &'a str,
    pub action: &'a str,
    pub project: &'a str,
    pub run: Option<&'a str>,
    pub outcome: Outcome,
    pub reason: Option<&'a str>,
}

fn path(roots: &StorageRoots) -> std::path::PathBuf {
    roots.state.join("remote-actions.jsonl")
}

/// Appends one line for `attempt`, made at `now` (seconds since the epoch).
pub fn record(roots: &StorageRoots, attempt: &Attempt, now: u64) -> Result<(), String> {
    let line = json!({
        "time": now,
        "identity": redact(attempt.identity),
        "action": redact(attempt.action),
        "project": redact(attempt.project),
        "run": attempt.run.map(redact),
        "outcome": match attempt.outcome { Outcome::Allowed => "allowed", Outcome::Refused => "refused" },
        "reason": attempt.reason.map(redact),
    });
    let mut file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path(roots)).map_err(|e| format!("cannot write the audit log: {}", e.kind()))?;
    file.write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("cannot write the audit log: {}", e.kind()))
}

/// The newest `limit` lines, newest first; no log yet reads as empty.
pub fn read(roots: &StorageRoots, limit: usize) -> Result<Vec<Value>, String> {
    let text = match std::fs::read_to_string(path(roots)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read the audit log: {}", e.kind())),
    };
    Ok(text.lines().rev().filter_map(|line| serde_json::from_str(line).ok()).take(limit).collect())
}
