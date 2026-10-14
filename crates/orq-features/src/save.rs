use std::fs::TryLockError;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{parse_ledger_bytes, write_ledger_atomic, FeatureEntry, LedgerError};

/// A ledger together with the exact bytes it was read from, so a later save can
/// tell whether anyone else changed the file in between.
#[derive(Debug)]
pub struct LoadedLedger {
    path: PathBuf,
    bytes: Vec<u8>,
    entries: Vec<FeatureEntry>,
}

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
        Ok(Self { path: path.to_path_buf(), bytes, entries })
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
            Ok(current) if current == self.bytes => {}
            Ok(_) => return Err(SaveError::Conflict),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SaveError::Conflict),
            Err(e) => return Err(SaveError::Io(e)),
        }
        write(&self.path, contents).map_err(SaveError::Io)
    }

    fn lock(&self, wait: Duration) -> Result<std::fs::File, SaveError> {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.path.with_file_name(name))
            .map_err(SaveError::Io)?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(LOCK_POLL),
                Err(TryLockError::WouldBlock) => return Err(SaveError::Locked),
                Err(TryLockError::Error(e)) => return Err(SaveError::Io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
