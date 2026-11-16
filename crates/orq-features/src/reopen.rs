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

/// [`reopen_features`] for a discarded run: its work is gone with it, so of `names` those in
/// `completed_too` also go back to `pending` when the ledger records them as `completed`. The caller
/// leaves out a feature whose `completed` this run cannot have written.
pub fn reopen_discarded_features(path: &Path, names: &[String], completed_too: &[String]) -> Result<Vec<String>, String> {
    reopen(path, names, &|status, name| is(status, FeatureStatus::InProgress) || (is(status, FeatureStatus::Completed) && completed_too.iter().any(|c| c == name)), FeatureStatus::Pending, &mut || {})
}

/// For a run that ended without delivering but left commits to resume: each of `names` that the
/// ledger at `path` records as `completed` goes back to `in_progress`, since the agent marked it
/// before its run was done. Saved as [`reopen_features`] saves; returns the names that were reopened.
pub fn reopen_unfinished_features(path: &Path, names: &[String]) -> Result<Vec<String>, String> {
    reopen(path, names, &|status, _| is(status, FeatureStatus::Completed), FeatureStatus::InProgress, &mut || {})
}

/// [`reopen_features`], calling `before_save` between each load and its save: a seam for tests
/// that race another writer against the save.
#[doc(hidden)]
pub fn reopen_features_before_save(path: &Path, names: &[String], before_save: &mut dyn FnMut()) -> Result<Vec<String>, String> {
    reopen(path, names, &|status, _| is(status, FeatureStatus::InProgress), FeatureStatus::Pending, before_save)
}

/// Closes the features a run's fix sessions started, at the run's end. With `to` `completed` (the run
/// completed) each of `names` the ledger at `path` records as `in_progress` becomes `completed`, with
/// `recordedAt` set to `recorded_at`; with `to` `abandoned` (the run was discarded) each one still
/// `in_progress` or `pending`, or already `completed` (its work goes with the run), becomes `abandoned`. Any other status, any other `to`, and any name the
/// ledger does not have are left alone, and so is every other byte of the file. Saved as
/// [`reopen_features`] saves; returns the names that were closed.
pub fn close_features(path: &Path, names: &[String], to: FeatureStatus, recorded_at: &str) -> Result<Vec<String>, String> {
    let closable = |status: &LedgerStatus, _: &str| match to {
        FeatureStatus::Completed => is(status, FeatureStatus::InProgress),
        FeatureStatus::Abandoned => {
            is(status, FeatureStatus::InProgress) || is(status, FeatureStatus::Pending) || is(status, FeatureStatus::Completed)
        }
        _ => false,
    };
    let stamp = |text: String, name: &str| with_recorded_at(&text, name, recorded_at);
    edit(path, names, &closable, &|text, closed| {
        let text = set_entry_statuses(text, closed, to).map_err(|e| format!("{e:?}"))?;
        if to != FeatureStatus::Completed {
            return Ok(text);
        }
        closed.iter().try_fold(text, |text, name| stamp(text, name))
    }, &mut || {})
}

/// For a run that delivered: each of `names` the ledger at `path` still records as `pending` or
/// `in_progress` becomes `completed`, with `recordedAt` set to `recorded_at`. Any other status, and
/// any name the ledger does not have, is left alone; saved as [`reopen_features`] saves. Returns the
/// names that were completed.
pub fn complete_run_features(path: &Path, names: &[String], recorded_at: &str) -> Result<Vec<String>, String> {
    let open = |status: &LedgerStatus, _: &str| is(status, FeatureStatus::Pending) || is(status, FeatureStatus::InProgress);
    edit(path, names, &open, &|text, completed| {
        let text = set_entry_statuses(text, completed, FeatureStatus::Completed).map_err(|e| format!("{e:?}"))?;
        completed.iter().try_fold(text, |text, name| with_recorded_at(&text, name, recorded_at))
    }, &mut || {})
}

/// `text` with the `recordedAt` of the entry `name` set to `at`. A member added to an entry laid out one
/// member per line goes on a line of its own, indented as the member before it.
fn with_recorded_at(text: &str, name: &str, at: &str) -> Result<String, String> {
    let value = serde_json::Value::from(at).to_string();
    let edited = crate::edit::set_entry_fields(text, name, &[("recordedAt", value.clone())]).map_err(|e| format!("{e:?}"))?;
    let start = text.bytes().zip(edited.bytes()).position(|(a, b)| a != b).unwrap_or(text.len());
    let inline = format!(",\"recordedAt\":{value}");
    if !edited[start..].starts_with(&inline) || !text[start..].starts_with('\n') {
        return Ok(edited);
    }
    let line = &text[text[..start].rfind('\n').map_or(0, |i| i + 1)..start];
    let indent = &line[..line.len() - line.trim_start().len()];
    Ok(format!("{},\n{indent}\"recordedAt\": {value}{}", &text[..start], &text[start..]))
}

fn is(status: &LedgerStatus, wanted: FeatureStatus) -> bool {
    *status == LedgerStatus::Known(wanted)
}

/// Sets each of `names` whose entry `reopenable` accepts (given its status and name) to `to`.
fn reopen(
    path: &Path,
    names: &[String],
    reopenable: &dyn Fn(&LedgerStatus, &str) -> bool,
    to: FeatureStatus,
    before_save: &mut dyn FnMut(),
) -> Result<Vec<String>, String> {
    edit(path, names, reopenable, &|text, reopened| set_entry_statuses(text, reopened, to).map_err(|e| format!("{e:?}")), before_save)
}

/// Applies `change` to the ledger text for those of `names` whose entry `editable` accepts (given its
/// status and name), under the ledger's lock, retrying from a fresh load when another writer won the race.
fn edit(
    path: &Path,
    names: &[String],
    editable: &dyn Fn(&LedgerStatus, &str) -> bool,
    change: &dyn Fn(&str, &[String]) -> Result<String, String>,
    before_save: &mut dyn FnMut(),
) -> Result<Vec<String>, String> {
    for _ in 0..CONFLICT_RETRIES {
        let loaded = match LoadedLedger::load(path) {
            Ok(loaded) => loaded,
            Err(LedgerError::Missing { .. }) => return Ok(Vec::new()),
            Err(err) => return Err(format!("cannot read {}: {err:?}", path.display())),
        };
        let accepted: HashSet<&str> = loaded
            .entries()
            .iter()
            .filter(|e| editable(&e.status, &e.feature_name))
            .map(|e| e.feature_name.as_str())
            .collect();
        let edited: Vec<String> = names.iter().filter(|name| accepted.contains(name.as_str())).cloned().collect();
        if edited.is_empty() {
            return Ok(edited);
        }
        let text = std::str::from_utf8(loaded.bytes()).map_err(|e| format!("{} is not UTF-8: {e}", path.display()))?;
        let text = change(text, &edited).map_err(|e| format!("cannot edit {}: {e}", path.display()))?;
        before_save();
        match loaded.save(&text) {
            Ok(()) => return Ok(edited),
            Err(SaveError::Conflict) => continue,
            Err(err) => return Err(format!("cannot save {}: {err:?}", path.display())),
        }
    }
    Err(format!("{} kept changing while its features were being edited", path.display()))
}
