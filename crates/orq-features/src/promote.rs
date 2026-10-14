use std::path::Path;
use std::time::Duration;

use orq_core::{check_transition, FeatureStatus, IllegalTransition};

use crate::{set_entry_status, EditError, FeatureEntry, LedgerError, LedgerStatus, LoadedLedger, SaveError};

/// Why a promotion did not happen. Every variant leaves the ledger unchanged.
#[derive(Debug)]
pub enum PromoteError {
    /// The ledger could not be loaded (missing, unreadable or malformed).
    Ledger(LedgerError),
    /// No entry carries the requested name.
    UnknownFeature(String),
    /// The entry's status is not one Orq knows, so it is never transitioned.
    UnrecognisedStatus(String),
    /// The transition policy refuses the change (anything but draft → pending).
    IllegalTransition(IllegalTransition),
    /// The file changed since it was loaded.
    Conflict,
    /// The ledger's lock was held for the whole wait.
    Locked,
    /// The file could not be inspected or written.
    Io(std::io::Error),
}

/// Promotes the feature `name` in the ledger at `path` from draft to pending:
/// load, check the transition policy, then save under the ledger's lock with
/// conflict detection. Returns the entry as committed, so a caller reports
/// what this call wrote rather than re-reading a ledger others may have changed.
pub fn promote_feature(path: &Path, name: &str) -> Result<FeatureEntry, PromoteError> {
    promote_feature_waiting(path, name, None)
}

/// Like [`promote_feature`], waiting at most `wait` for the lock (the default
/// wait when `None`).
pub fn promote_feature_waiting(
    path: &Path,
    name: &str,
    wait: Option<Duration>,
) -> Result<FeatureEntry, PromoteError> {
    let loaded = LoadedLedger::load(path).map_err(PromoteError::Ledger)?;
    promote_loaded(&loaded, name, wait)
}

impl LoadedLedger {
    /// Promotes `name` from draft to pending in the ledger as it was loaded:
    /// if the file has changed since, nothing is written and the result is
    /// [`PromoteError::Conflict`]. Waits at most `wait` for the lock (the
    /// default wait when `None`).
    pub fn promote(&self, name: &str, wait: Option<Duration>) -> Result<FeatureEntry, PromoteError> {
        promote_loaded(self, name, wait)
    }
}

pub(crate) fn promote_loaded(
    loaded: &LoadedLedger,
    name: &str,
    wait: Option<Duration>,
) -> Result<FeatureEntry, PromoteError> {
    let entry = loaded
        .entries()
        .iter()
        .find(|entry| entry.feature_name == name)
        .ok_or_else(|| PromoteError::UnknownFeature(name.to_string()))?;
    let from = match &entry.status {
        LedgerStatus::Known(status) => *status,
        LedgerStatus::Unrecognised(text) => return Err(PromoteError::UnrecognisedStatus(text.clone())),
    };
    check_transition(from, FeatureStatus::Pending).map_err(PromoteError::IllegalTransition)?;
    let text = std::str::from_utf8(loaded.bytes())
        .map_err(|e| PromoteError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    let edited = set_entry_status(text, name, FeatureStatus::Pending).map_err(|e| match e {
        EditError::UnknownFeature => PromoteError::UnknownFeature(name.to_string()),
        EditError::Malformed => PromoteError::Ledger(LedgerError::Malformed {
            path: loaded.path().to_path_buf(),
            reason: "entry has no editable status".to_string(),
            position: None,
        }),
    })?;
    let saved = match wait {
        Some(wait) => loaded.save_waiting(&edited, wait),
        None => loaded.save(&edited),
    };
    saved.map_err(|e| match e {
        SaveError::Conflict => PromoteError::Conflict,
        SaveError::Locked => PromoteError::Locked,
        SaveError::Io(e) => PromoteError::Io(e),
    })?;
    let mut committed = entry.clone();
    committed.status = LedgerStatus::Known(FeatureStatus::Pending);
    Ok(committed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn an_external_edit_between_load_and_save_is_a_conflict_and_unchanged() {
        let dir = std::env::temp_dir().join(format!("orq-promote-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".tdd-features.json");
        std::fs::write(&path, r#"[{"featureName":"a","status":"draft"}]"#).unwrap();
        let loaded = LoadedLedger::load(&path).unwrap();
        let external = r#"[{"featureName":"a","status":"draft"},{"featureName":"b","status":"draft"}]"#;
        std::fs::write(&path, external).unwrap();

        let result = promote_loaded(&loaded, "a", None);

        assert!(matches!(result, Err(PromoteError::Conflict)), "{result:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
    }
}
