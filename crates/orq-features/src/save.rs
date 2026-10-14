use std::path::{Path, PathBuf};

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
    /// The file could not be inspected or written.
    Io(std::io::Error),
}

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
    pub fn save(&self, contents: &str) -> Result<(), SaveError> {
        match std::fs::read(&self.path) {
            Ok(current) if current == self.bytes => {}
            Ok(_) => return Err(SaveError::Conflict),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SaveError::Conflict),
            Err(e) => return Err(SaveError::Io(e)),
        }
        write_ledger_atomic(&self.path, contents).map_err(SaveError::Io)
    }
}
