use std::collections::HashSet;
use std::path::Path;

use orq_core::FeatureStatus;

use crate::{set_entry_statuses, LedgerError, LedgerStatus, LoadedLedger, SaveError};

/// How many times a save that lost a race to another writer is retried from a fresh load.
const CONFLICT_RETRIES: usize = 3;

/// Puts each of `names` that the ledger at `path` records as `in_progress` back to `pending`, so
/// a feature whose run left nothing to continue can be launched again. Any other status, and
/// any name the ledger does not have, is left alone; so is every other byte of the file. A
/// missing ledger reopens nothing and is not created; when nothing needs reopening nothing is
/// written. The save happens under the ledger's lock, and one that loses a race to another
/// writer starts again from what that writer left. Returns the names that were reopened.
///
/// This deliberately bypasses the user-facing transition policy (which only promotes drafts):
/// it undoes what the agent's TDD server recorded, not a choice the user made.
pub fn reopen_features(path: &Path, names: &[String]) -> Result<Vec<String>, String> {
    reopen_features_before_save(path, names, &mut || {})
}

/// [`reopen_features`], calling `before_save` between each load and its save: a seam for tests
/// that race another writer against the save.
#[doc(hidden)]
pub fn reopen_features_before_save(path: &Path, names: &[String], before_save: &mut dyn FnMut()) -> Result<Vec<String>, String> {
    for _ in 0..CONFLICT_RETRIES {
        let loaded = match LoadedLedger::load(path) {
            Ok(loaded) => loaded,
            Err(LedgerError::Missing { .. }) => return Ok(Vec::new()),
            Err(err) => return Err(format!("cannot read {}: {err:?}", path.display())),
        };
        let in_progress: HashSet<&str> = loaded
            .entries()
            .iter()
            .filter(|e| e.status == LedgerStatus::Known(FeatureStatus::InProgress))
            .map(|e| e.feature_name.as_str())
            .collect();
        let reopened: Vec<String> = names.iter().filter(|name| in_progress.contains(name.as_str())).cloned().collect();
        if reopened.is_empty() {
            return Ok(reopened);
        }
        let text = std::str::from_utf8(loaded.bytes()).map_err(|e| format!("{} is not UTF-8: {e}", path.display()))?;
        let text = set_entry_statuses(text, &reopened, FeatureStatus::Pending).map_err(|e| format!("cannot edit {}: {e:?}", path.display()))?;
        before_save();
        match loaded.save(&text) {
            Ok(()) => return Ok(reopened),
            Err(SaveError::Conflict) => continue,
            Err(err) => return Err(format!("cannot save {}: {err:?}", path.display())),
        }
    }
    Err(format!("{} kept changing while its features were being reopened", path.display()))
}
