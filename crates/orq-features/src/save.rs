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
