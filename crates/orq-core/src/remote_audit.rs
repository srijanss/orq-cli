//! The audit log of remote actions: one JSON line appended for every attempt, allowed or refused, saying
//! when, who (the signed-in passkey's label), what, and on which project and run. An attempt is built
//! only from these named fields, so no header or token can reach the file, and free text is redacted
//! on the way in. Nothing here rewrites or removes a line; the web reads the log and cannot change it.

use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

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
    let mut file = open(roots, true)?;
    // A write that was cut short leaves a line with no end; the next record starts a line of its own.
    let torn = file.metadata().map(|meta| meta.len() > 0).unwrap_or(false) && !ends_with_newline(roots);
    file.write_all(format!("{}{line}\n", if torn { "\n" } else { "" }).as_bytes()).map_err(|e| format!("cannot write the audit log: {}", e.kind()))
}

fn ends_with_newline(roots: &StorageRoots) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = open(roots, false) else { return false };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).and_then(|_| file.read_exact(&mut last)).is_ok() && last[0] == b'\n'
}

/// Opens the log without following a link, and only if it is a regular file this user owns; a file
/// left readable by others is tightened to this user alone.
fn open(roots: &StorageRoots, write: bool) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.custom_flags(libc::O_NOFOLLOW);
    if write {
        options.create(true).append(true).mode(0o600);
    } else {
        options.read(true);
    }
    let file = options.open(path(roots)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "not found".to_string(),
        kind => format!("cannot open the audit log: {kind}"),
    })?;
    let meta = file.metadata().map_err(|e| format!("cannot open the audit log: {}", e.kind()))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err("the audit log is not a plain file of this user".to_string());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| format!("cannot open the audit log: {}", e.kind()))?;
    }
    Ok(file)
}

/// The newest `limit` lines, newest first; no log yet reads as empty.
pub fn read(roots: &StorageRoots, limit: usize) -> Result<Vec<Value>, String> {
    let mut text = String::new();
    match open(roots, false) {
        Ok(mut file) => std::io::Read::read_to_string(&mut file, &mut text).map_err(|e| format!("cannot read the audit log: {}", e.kind()))?,
        Err(why) if why == "not found" => return Ok(Vec::new()),
        Err(why) => return Err(why),
    };
    Ok(text.lines().rev().filter(|line| !line.is_empty()).map(|line| serde_json::from_str(line).unwrap_or_else(|_| json!({ "damaged": true }))).take(limit).collect())
}
