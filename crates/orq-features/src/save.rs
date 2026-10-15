use std::fs::TryLockError;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{
    parse_ledger, parse_ledger_bytes, validate_entries, write_ledger_atomic, FeatureDraft, FeatureEntry,
    LedgerError, ValidationError,
};

/// A ledger together with the exact bytes it was read from, so a later save can
/// tell whether anyone else changed the file in between.
#[derive(Debug)]
pub struct LoadedLedger {
    path: PathBuf,
    bytes: Vec<u8>,
    entries: Vec<FeatureEntry>,
    /// The file did not exist when loaded, so a save must find it still absent.
    is_new: bool,
}

/// Why a feature could not be appended.
#[derive(Debug)]
pub enum AppendError {
    /// The resulting array would be invalid; nothing was written.
    Invalid(ValidationError),
    /// The write did not happen.
    Save(SaveError),
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::Invalid(e) => write!(f, "{e}"),
            AppendError::Save(SaveError::Conflict) => {
                write!(f, "the ledger changed since it was loaded; reload and review before saving")
            }
            AppendError::Save(SaveError::Locked) => write!(f, "the ledger is locked by another process"),
            AppendError::Save(SaveError::Io(e)) => write!(f, "could not write the ledger: {e}"),
        }
    }
}

impl std::error::Error for AppendError {}

/// The fields `edit_fields` may change; `None` leaves a field as it is.
/// `featureName` and `status` are deliberately not here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldEdits {
    pub description: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub parallelizable: Option<bool>,
    pub test_file: Option<String>,
    pub target_files: Option<Vec<String>>,
}

/// Why fields could not be edited.
#[derive(Debug)]
pub enum EditFieldsError {
    /// No entry carries that featureName.
    UnknownFeature,
    /// The ledger text could not be edited safely.
    Malformed,
    /// The resulting array would be invalid; nothing was written.
    Invalid(ValidationError),
    /// The write did not happen.
    Save(SaveError),
}

impl std::fmt::Display for EditFieldsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditFieldsError::UnknownFeature => write!(f, "no feature with that name"),
            EditFieldsError::Malformed => write!(f, "the ledger could not be edited safely"),
            EditFieldsError::Invalid(e) => write!(f, "{e}"),
            EditFieldsError::Save(SaveError::Conflict) => {
                write!(f, "the ledger changed since it was loaded; reload and review before saving")
            }
            EditFieldsError::Save(SaveError::Locked) => write!(f, "the ledger is locked by another process"),
            EditFieldsError::Save(SaveError::Io(e)) => write!(f, "could not write the ledger: {e}"),
        }
    }
}

impl std::error::Error for EditFieldsError {}

/// Why a save did not happen.
#[derive(Debug)]
pub enum SaveError {
    /// The file changed or disappeared since it was loaded; nothing was written.
    Conflict,
    /// Another process held the ledger's lock for the whole wait; nothing was
    /// written.
    Locked,
    /// The file could not be inspected or written.
    Io(std::io::Error),
}

/// How long a save waits for the ledger's lock before giving up.
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(10);

impl LoadedLedger {
    /// Reads and validates the ledger at `path`, remembering its bytes.
    pub fn load(path: &Path) -> Result<Self, LedgerError> {
        let bytes = std::fs::read(path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => LedgerError::Missing { path: path.to_path_buf() },
            _ => LedgerError::Unreadable { path: path.to_path_buf(), source },
        })?;
        let entries = parse_ledger_bytes(path, &bytes)?;
        Ok(Self { path: path.to_path_buf(), bytes, entries, is_new: false })
    }

    /// Like [`load`](Self::load), but a missing file is a valid empty ledger
    /// that a later save creates. Any other failure (unreadable, not an array,
    /// malformed) is returned as is and nothing is written.
    pub fn load_or_new(path: &Path) -> Result<Self, LedgerError> {
        match Self::load(path) {
            Err(LedgerError::Missing { .. }) => Ok(Self::absent(path)),
            other => other,
        }
    }

    /// The snapshot of a ledger observed as missing, without reading the path
    /// again: a file that exists by the time of a save is a conflict.
    pub fn absent(path: &Path) -> Self {
        Self { path: path.to_path_buf(), bytes: Vec::new(), entries: Vec::new(), is_new: true }
    }

    /// Whether the file was absent when loaded.
    pub fn is_new(&self) -> bool {
        self.is_new
    }

    /// Appends `draft` as a new last entry, after validating the whole
    /// resulting array. Only the new entry's text is added: every byte of the
    /// existing entries is kept, and an absent file is created as a one-entry
    /// array. Returns the entry as saved.
    pub fn append(&self, draft: &FeatureDraft) -> Result<FeatureEntry, AppendError> {
        let entry_json = draft.to_json();
        let new_entry = parse_ledger(&format!("[{entry_json}]"))
            .ok()
            .and_then(|mut v| v.pop())
            .expect("a FeatureDraft always serialises to a valid entry");
        let mut candidate = self.entries.clone();
        candidate.push(new_entry.clone());
        validate_entries(&candidate).map_err(AppendError::Invalid)?;

        let text = std::str::from_utf8(&self.bytes).expect("a parsed ledger is UTF-8");
        let contents = if self.is_new {
            format!("[\n  {entry_json}\n]\n")
        } else {
            let close = text.rfind(']').expect("a parsed ledger ends with ]");
            let last = text[..close].trim_end_matches(|c: char| c.is_whitespace()).len();
            let separator = if text[..last].ends_with('[') { "\n  " } else { ",\n  " };
            let tail = &text[last..];
            // A one-line array keeps `]` where it was; a multi-line one keeps its own layout.
            format!("{}{separator}{entry_json}{}", &text[..last], if tail.starts_with('\n') || tail.starts_with("\r") { tail } else { "\n]\n" })
        };
        self.save(&contents).map_err(AppendError::Save)?;
        Ok(new_entry)
    }

    /// Changes the given fields of the feature named `feature_name`, keeping
    /// every other byte of the ledger (unknown fields, status, other entries).
    /// The whole resulting array is validated before the atomic save; a file
    /// changed since load is a conflict. Returns the entry as saved.
    pub fn edit_fields(&self, feature_name: &str, edits: &FieldEdits) -> Result<FeatureEntry, EditFieldsError> {
        let current = self
            .entries
            .iter()
            .find(|e| e.feature_name == feature_name)
            .ok_or(EditFieldsError::UnknownFeature)?;
        let json = |value: serde_json::Value| value.to_string();
        let mut members: Vec<(&str, String)> = Vec::new();
        if let Some(v) = &edits.description {
            members.push(("description", json(v.as_str().into())));
        }
        if let Some(v) = &edits.depends_on {
            members.push(("dependsOn", json(serde_json::json!(v))));
        }
        if let Some(v) = &edits.test_file {
            members.push(("testFile", json(v.as_str().into())));
        }
        if let Some(v) = &edits.target_files {
            members.push(("targetFiles", json(serde_json::json!(v))));
        }
        if let Some(v) = edits.parallelizable {
            members.push(("parallelizable", json(v.into())));
        }
        if members.is_empty() {
            return Ok(current.clone());
        }
        let text = std::str::from_utf8(&self.bytes).map_err(|_| EditFieldsError::Malformed)?;
        let edited = crate::edit::set_entry_fields(text, feature_name, &members).map_err(|e| match e {
            crate::EditError::UnknownFeature => EditFieldsError::UnknownFeature,
            crate::EditError::Malformed => EditFieldsError::Malformed,
        })?;
        let entries = parse_ledger_bytes(&self.path, edited.as_bytes()).map_err(|_| EditFieldsError::Malformed)?;
        validate_entries(&entries).map_err(EditFieldsError::Invalid)?;
        let saved = entries
            .into_iter()
            .find(|e| e.feature_name == feature_name)
            .ok_or(EditFieldsError::Malformed)?;
        self.save(&edited).map_err(EditFieldsError::Save)?;
        Ok(saved)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[FeatureEntry] {
        &self.entries
    }

    /// Writes `contents` atomically, but only if the file still holds exactly
    /// the bytes this ledger was loaded from; otherwise returns
    /// [`SaveError::Conflict`] and leaves the file alone.
    ///
    /// The comparison and the write happen under the ledger's advisory lock
    /// (an exclusive `flock` on the sibling `<ledger>.lock` file, the same one
    /// the TDD MCP takes), so cooperating writers cannot interleave between
    /// them. A writer that ignores the lock is not excluded. If another
    /// process holds the lock for the whole wait, returns [`SaveError::Locked`].
    ///
    /// Saving is supported on Unix only; elsewhere it returns an
    /// [`SaveError::Io`] of kind `Unsupported`.
    pub fn save(&self, contents: &str) -> Result<(), SaveError> {
        self.save_waiting(contents, LOCK_WAIT)
    }

    /// Like [`save`](Self::save), waiting at most `wait` for the lock.
    pub fn save_waiting(&self, contents: &str, wait: Duration) -> Result<(), SaveError> {
        self.save_with(contents, wait, &write_ledger_atomic)
    }

    fn save_with(
        &self,
        contents: &str,
        wait: Duration,
        write: &dyn Fn(&Path, &str) -> std::io::Result<()>,
    ) -> Result<(), SaveError> {
        // Dropping the handle releases the lock, so every return below does.
        let _lock = self.lock(wait)?;
        match std::fs::read(&self.path) {
            Ok(current) if !self.is_new && current == self.bytes => {}
            Ok(_) => return Err(SaveError::Conflict),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !self.is_new {
                    return Err(SaveError::Conflict);
                }
            }
            Err(e) => return Err(SaveError::Io(e)),
        }
        write(&self.path, contents).map_err(SaveError::Io)
    }

    fn lock(&self, wait: Duration) -> Result<std::fs::File, SaveError> {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let lock_path = self.path.with_file_name(name);
        let file = open_lock_file(&lock_path).map_err(SaveError::Io)?;
        poll_lock(&mut || file.try_lock(), &Instant::now, &std::thread::sleep, wait)?;
        Ok(file)
    }
}

/// Opens the lock file without ever blocking or following a symlink: a FIFO
/// planted at the path would otherwise stall the open (`O_NONBLOCK` makes it
/// fail instead), and the descriptor itself is checked to be a regular file,
/// so there is no window between a path check and the open.
#[cfg(unix)]
fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ledger lock path is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_lock_file(_path: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "ledger locking is only supported on Unix"))
}

/// Retries `try_lock` until it succeeds or `wait` has elapsed. The clock and
/// the sleeper are injected so the deadline behaviour can be tested exactly.
fn poll_lock(
    try_lock: &mut dyn FnMut() -> Result<(), TryLockError>,
    now: &dyn Fn() -> Instant,
    sleep: &dyn Fn(Duration),
    wait: Duration,
) -> Result<(), SaveError> {
    let deadline = now() + wait;
    loop {
        match try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(now());
                if remaining.is_zero() {
                    return Err(SaveError::Locked);
                }
                sleep(LOCK_POLL.min(remaining));
                if now() >= deadline {
                    return Err(SaveError::Locked);
                }
            }
            Err(TryLockError::Error(e)) => return Err(SaveError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_ends_at_the_deadline_even_if_the_lock_frees_right_after_it() {
        use std::cell::Cell;
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        // The lock frees 5ms in; the caller only waits 1ms.
        let mut try_lock = || {
            if elapsed.get() >= Duration::from_millis(5) { Ok(()) } else { Err(TryLockError::WouldBlock) }
        };

        let result = poll_lock(
            &mut try_lock,
            &|| start + elapsed.get(),
            &|d| elapsed.set(elapsed.get() + d),
            Duration::from_millis(1),
        );

        assert!(matches!(result, Err(SaveError::Locked)), "{result:?}");
        assert_eq!(elapsed.get(), Duration::from_millis(1));
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_write_is_an_io_error_not_a_conflict() {
        let dir = std::env::temp_dir().join(format!("orq-save-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".tdd-features.json");
        std::fs::write(&path, "[]").unwrap();
        let loaded = LoadedLedger::load(&path).unwrap();

        let result = loaded.save_with("[1]", Duration::ZERO, &|_, _| Err(std::io::Error::other("injected")));

        assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_is_held_during_the_write_and_free_after_a_failed_one() {
        let dir = std::env::temp_dir().join(format!("orq-save-critical-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".tdd-features.json");
        std::fs::write(&path, "[]").unwrap();
        let loaded = LoadedLedger::load(&path).unwrap();
        let probe = || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(dir.join(".tdd-features.json.lock"))
                .unwrap();
            matches!(file.try_lock(), Err(TryLockError::WouldBlock))
        };

        let result = loaded.save_with("[1]", Duration::ZERO, &|_, _| {
            assert!(probe(), "lock not held during the write");
            Err(std::io::Error::other("injected"))
        });

        assert!(matches!(result, Err(SaveError::Io(_))));
        assert!(!probe(), "lock still held after the failed write");
    }
}
