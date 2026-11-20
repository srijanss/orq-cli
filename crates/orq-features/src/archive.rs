//! Moving old completed features out of the ledger into a per-project archive.
//!
//! The ledger is handled as raw JSON values so fields this version does not know survive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::write_ledger_atomic;

/// What an archive pass moved (or would move, on a dry run).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchivePlan {
    /// Names moved to the archive, in ledger order.
    pub archived: Vec<String>,
    /// Remaining features whose `dependsOn` lost archived names: (feature, names removed).
    pub stripped: Vec<(String, Vec<String>)>,
    /// Whether nothing was written because of a dry run.
    pub dry_run: bool,
}

#[derive(Debug)]
pub enum ArchiveError {
    /// A run of the project is active, so the ledger must not be rewritten.
    ActiveRun,
    /// The ledger does not parse or does not round-trip.
    Ledger(String),
    /// The archive file is unreadable or not a JSON array.
    Archive(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::ActiveRun => write!(f, "a run of this project is active; wait for it to finish before archiving"),
            ArchiveError::Ledger(m) => write!(f, "the ledger cannot be archived: {m}"),
            ArchiveError::Archive(m) => write!(f, "the archive file is unusable: {m}"),
            ArchiveError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e)
    }
}

/// `<data dir>/archive/<project id>/features.json`.
pub fn archive_path(data_root: &Path, project_id: &str) -> PathBuf {
    data_root.join("archive").join(project_id).join("features.json")
}

/// The archive's entries; an absent file is an empty archive.
pub fn read_archive(path: &Path) -> Result<Vec<Value>, ArchiveError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ArchiveError::Io(e)),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(ArchiveError::Archive("not a JSON array".into())),
        Err(e) => Err(ArchiveError::Archive(e.to_string())),
    }
}

/// The names of archived features that are completed (what a `dependsOn` may still name).
/// An unreadable archive counts as empty.
pub fn archived_completed_names(data_root: &Path, project_id: &str) -> HashSet<String> {
    let Ok(entries) = read_archive(&archive_path(data_root, project_id)) else { return HashSet::new() };
    entries
        .iter()
        .filter(|e| e.get("status").and_then(Value::as_str) == Some("completed"))
        .filter_map(|e| e.get("featureName").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn name_of(entry: &Value) -> Option<&str> {
    entry.get("featureName").and_then(Value::as_str)
}

fn status_of(entry: &Value) -> Option<&str> {
    entry.get("status").and_then(Value::as_str)
}

fn deps_of(entry: &Value) -> Vec<String> {
    entry
        .get("dependsOn")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Moves every completed feature except the newest `keep` (by ledger position), and every
/// abandoned feature no remaining feature still depends on, into the project's archive, and
/// removes the moved names from the remaining `dependsOn` lists.
///
/// Refuses while `active_runs > 0` or when the ledger does not parse as feature entries. The
/// ledger is snapshotted first. The archive is written before the ledger, so a failure in between
/// leaves a duplicate in the archive rather than a lost feature.
pub fn archive_ledger(
    ledger: &Path,
    data_root: &Path,
    project_id: &str,
    keep: usize,
    dry_run: bool,
    active_runs: usize,
    now_iso: &str,
) -> Result<ArchivePlan, ArchiveError> {
    if active_runs > 0 {
        return Err(ArchiveError::ActiveRun);
    }
    // The ledger is read as a LoadedLedger so the final write goes through its lock and its unchanged-bytes check.
    let loaded = crate::LoadedLedger::load(ledger).map_err(|e| ArchiveError::Ledger(format!("{e:?}")))?;
    let bytes = loaded.bytes().to_vec();
    let entries = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Array(items)) => items,
        Ok(_) => return Err(ArchiveError::Ledger("not a JSON array".into())),
        Err(e) => return Err(ArchiveError::Ledger(e.to_string())),
    };
    if let Err(e) = crate::parse_ledger(&String::from_utf8_lossy(&bytes)) {
        return Err(ArchiveError::Ledger(format!("does not parse as feature entries: {e}")));
    }

    let completed: Vec<usize> = (0..entries.len()).filter(|&i| status_of(&entries[i]) == Some("completed")).collect();
    let kept_from = completed.len().saturating_sub(keep);
    let mut moving: Vec<bool> = vec![false; entries.len()];
    for &i in &completed[..kept_from] {
        moving[i] = true;
    }
    for (i, entry) in entries.iter().enumerate() {
        if status_of(entry) == Some("abandoned") {
            moving[i] = true;
        }
    }
    // An abandoned feature a remaining feature still waits on stays, so stripping never quietly
    // satisfies a dependency that was abandoned.
    loop {
        let needed: HashSet<String> =
            (0..entries.len()).filter(|&i| !moving[i]).flat_map(|i| deps_of(&entries[i])).collect();
        let mut changed = false;
        for i in 0..entries.len() {
            if moving[i] && status_of(&entries[i]) == Some("abandoned") && name_of(&entries[i]).is_some_and(|n| needed.contains(n)) {
                moving[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let archived_names: HashSet<String> =
        (0..entries.len()).filter(|&i| moving[i]).filter_map(|i| name_of(&entries[i]).map(str::to_string)).collect();
    let mut plan = ArchivePlan { dry_run, ..ArchivePlan::default() };
    let mut moved: Vec<Value> = Vec::new();
    let mut remaining: Vec<Value> = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        if moving[i] {
            plan.archived.push(name_of(&entry).unwrap_or_default().to_string());
            let mut entry = entry;
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("archivedAt".into(), json!(now_iso));
            }
            moved.push(entry);
            continue;
        }
        let mut entry = entry;
        let deps = deps_of(&entry);
        let (gone, left): (Vec<String>, Vec<String>) = deps.into_iter().partition(|d| archived_names.contains(d));
        if !gone.is_empty() {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("dependsOn".into(), json!(left));
            }
            plan.stripped.push((name_of(&entry).unwrap_or_default().to_string(), gone));
        }
        remaining.push(entry);
    }

    if plan.archived.is_empty() || dry_run {
        return Ok(plan);
    }

    let now_secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    orq_core::storage::snapshot_ledger(data_root, project_id, &bytes, now_secs)?;

    let path = archive_path(data_root, project_id);
    let moved_records: Vec<Value> = moved
        .into_iter()
        .chain(plan.stripped.iter().map(|(name, gone)| json!({"featureName": name, "archivedDependsOn": gone})))
        .collect();
    // Under the ledger's lock, once the ledger is found unchanged since it was read: the archive is appended
    // first, then the ledger replaced, so concurrent writers and a second archive pass see one or the other
    // whole. A change to the ledger since it was read leaves both files as they were.
    let archive_first = || -> std::io::Result<()> {
        let mut archive = read_archive(&path).map_err(|e| std::io::Error::other(e.to_string()))?;
        archive.extend(moved_records.iter().cloned());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_ledger_atomic(&path, &(serde_json::to_string_pretty(&Value::Array(archive)).expect("json") + "\n"))
    };
    let text = serde_json::to_string_pretty(&Value::Array(remaining)).expect("json") + "\n";
    loaded.save_after(&text, &archive_first).map_err(|e| match e {
        crate::SaveError::Conflict => ArchiveError::Ledger("the ledger changed while it was being archived; nothing was written, try again".into()),
        crate::SaveError::Locked => ArchiveError::Ledger("the ledger is locked by another writer; nothing was written, try again".into()),
        crate::SaveError::Io(e) => ArchiveError::Io(e),
    })?;
    Ok(plan)
}
