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
    /// A run of the project is in its implement step, so a completed feature is not reopened by hand.
    Implementing,
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

/// `orq feature set-status --status pending --reopen`: moves the completed feature `name` in the ledger
/// at `path` back to pending, saved as [`promote_feature`] saves. Only completed goes back this way
/// (anything else is [`PromoteError::IllegalTransition`]), and nothing is written while a run of the
/// project is in its implement step (`implementing`), whose session may be changing the ledger.
pub fn reopen_completed_feature(path: &Path, name: &str, implementing: bool) -> Result<FeatureEntry, PromoteError> {
    if implementing {
        return Err(PromoteError::Implementing);
    }
    let loaded = LoadedLedger::load(path).map_err(PromoteError::Ledger)?;
    set_pending(&loaded, name, None, &|from| match from {
        FeatureStatus::Completed => Ok(()),
        _ => Err(IllegalTransition { from, to: FeatureStatus::Pending }),
    })
}

/// The stage a run's implement session runs in.
const IMPLEMENT_STAGE: &str = "implement_and_review";

/// Whether a run whose events are `events` (each its kind and stage, oldest first) is in its implement
/// step: its implement stage started and neither ended nor gave way to another stage, and the run did
/// not end or pause since.
pub fn in_implement_step<'a>(events: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> bool {
    let mut implementing = false;
    for (kind, stage) in events {
        if kind == "stage.started" {
            implementing = stage == Some(IMPLEMENT_STAGE);
        } else if kind.starts_with("stage.") && stage == Some(IMPLEMENT_STAGE) {
            implementing = false;
        } else if matches!(kind, "run.completed" | "run.failed" | "run.blocked" | "run.paused") {
            implementing = false;
        }
    }
    implementing
}

pub(crate) fn promote_loaded(
    loaded: &LoadedLedger,
    name: &str,
    wait: Option<Duration>,
) -> Result<FeatureEntry, PromoteError> {
    set_pending(loaded, name, wait, &|from| check_transition(from, FeatureStatus::Pending))
}

/// Sets `name` to pending in the ledger as it was loaded, when `allowed` accepts its status.
fn set_pending(
    loaded: &LoadedLedger,
    name: &str,
    wait: Option<Duration>,
    allowed: &dyn Fn(FeatureStatus) -> Result<(), IllegalTransition>,
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
    allowed(from).map_err(PromoteError::IllegalTransition)?;
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

/// The event recording that a completed feature went back to pending because the run's review
/// ledger has the finding of the same id open again.
pub const FINDING_REOPENED: &str = "ledger.finding_reopened";

/// Where the TDD tooling records each review round, and the findings still open, per review scope.
const ROUNDS_FILE: &str = ".tdd-review-rounds.json";
const FINDINGS_FILE: &str = ".tdd-review-findings.json";
/// A review ledger is a few kilobytes; anything far larger is not one and is not read whole.
const MAX_REVIEW_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
/// How many times a save that lost a race to another writer is retried from a fresh load.
const CONFLICT_RETRIES: usize = 3;

/// A finding orq reopened: its feature (the finding's own name) went from completed to pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReopenedFinding {
    pub feature: String,
    pub finding: String,
    /// The latest review round that has the finding open; 0 when no round names it.
    pub round: u64,
}

impl ReopenedFinding {
    /// The payload of its [`FINDING_REOPENED`] event.
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({"feature": self.feature, "finding": self.finding, "round": self.round})
    }
}

/// Before an implement session: each finding the review ledger in `worktree` has open whose id is a
/// feature the ledger at `ledger` records as completed goes back to pending, so the session can fix
/// it under its own name, and `record` is called with [`FINDING_REOPENED`] and its payload once the
/// ledger is saved. Every other feature, and every other byte of the ledger, is left alone; no review
/// ledger, or no main ledger, reopens nothing. Returns the findings reopened, by name.
pub fn reopen_open_findings(
    ledger: &Path,
    worktree: &Path,
    record: &mut dyn FnMut(&str, serde_json::Value),
) -> Result<Vec<ReopenedFinding>, String> {
    let Some(open) = read_review_ledger(&worktree.join(FINDINGS_FILE))? else {
        return Ok(Vec::new());
    };
    let rounds = read_review_ledger(&worktree.join(ROUNDS_FILE))?;
    let open = open_findings(&open, rounds.as_ref());
    if open.is_empty() {
        return Ok(Vec::new());
    }
    for _ in 0..CONFLICT_RETRIES {
        let loaded = match LoadedLedger::load(ledger) {
            Ok(loaded) => loaded,
            Err(LedgerError::Missing { .. }) => return Ok(Vec::new()),
            Err(err) => return Err(format!("cannot read {}: {err:?}", ledger.display())),
        };
        let completed = |name: &str| {
            loaded.entries().iter().any(|e| e.feature_name == name && e.status == LedgerStatus::Known(FeatureStatus::Completed))
        };
        let reopened: Vec<ReopenedFinding> = open
            .iter()
            .filter(|(id, _)| completed(id))
            .map(|(id, round)| ReopenedFinding { feature: id.clone(), finding: id.clone(), round: *round })
            .collect();
        if reopened.is_empty() {
            return Ok(reopened);
        }
        let names: Vec<String> = reopened.iter().map(|r| r.feature.clone()).collect();
        let text = std::str::from_utf8(loaded.bytes()).map_err(|e| format!("{} is not UTF-8: {e}", ledger.display()))?;
        let text = crate::set_entry_statuses(text, &names, FeatureStatus::Pending).map_err(|e| format!("cannot edit {}: {e:?}", ledger.display()))?;
        match loaded.save(&text) {
            Ok(()) => {
                for finding in &reopened {
                    record(FINDING_REOPENED, finding.payload());
                }
                return Ok(reopened);
            }
            Err(SaveError::Conflict) => continue,
            Err(err) => return Err(format!("cannot save {}: {err:?}", ledger.display())),
        }
    }
    Err(format!("{} kept changing while reopened findings were being set to pending", ledger.display()))
}

/// The ids of the findings `findings` (the review ledger, by scope) has open, each once, in order,
/// with the latest round of its scope in `rounds` that names it.
fn open_findings(findings: &serde_json::Value, rounds: Option<&serde_json::Value>) -> Vec<(String, u64)> {
    use serde_json::Value;
    let mut open: Vec<(String, u64)> = Vec::new();
    for (scope, entries) in findings.as_object().into_iter().flatten() {
        let scope_rounds = rounds.and_then(|r| r.get(scope)).and_then(Value::as_array);
        for entry in entries.as_array().into_iter().flatten() {
            if entry.get("status").and_then(Value::as_str) != Some("open") {
                continue;
            }
            let Some(id) = entry.get("id").and_then(Value::as_str) else { continue };
            let round = scope_rounds
                .into_iter()
                .flatten()
                .filter(|round| {
                    round.get("findings").and_then(Value::as_array).into_iter().flatten().any(|f| f.get("id").and_then(Value::as_str) == Some(id))
                })
                .filter_map(|round| round.get("round").and_then(Value::as_u64))
                .max()
                .unwrap_or(0);
            match open.iter_mut().find(|(known, _)| known == id) {
                Some(known) => known.1 = known.1.max(round),
                None => open.push((id.to_string(), round)),
            }
        }
    }
    open
}

/// A review ledger file, `None` when there is none. Opened without following a link and without
/// waiting on a pipe, then judged by the handle.
fn read_review_ledger(path: &Path) -> Result<Option<serde_json::Value>, String> {
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot open {name}: {e}")),
    };
    let meta = file.metadata().map_err(|e| format!("cannot read {name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REVIEW_LEDGER_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("cannot read {name}: {e}"))?;
    if bytes.len() as u64 > MAX_REVIEW_LEDGER_BYTES {
        return Err(format!("{name} is larger than {MAX_REVIEW_LEDGER_BYTES} bytes"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| format!("{name} is not valid JSON: {e}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn an_external_edit_between_load_and_save_is_a_conflict_and_unchanged() {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-promote-conflict-{}", std::process::id()));
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
