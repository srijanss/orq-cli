use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// A ledger that did not exist yet is created private; an existing one keeps
/// its permission bits.
const NEW_LEDGER_MODE: u32 = 0o600;

/// Replaces the ledger at `path` with `contents` in one step: the bytes go to a
/// temp file in the same directory, which is then renamed over the original,
/// so a reader sees the old file or the new one, never a partial write.
///
/// The original's permission bits carry over. A symlinked ledger is written
/// through to its target, so the link itself stays a link. On any failure the
/// original is untouched and this call's temp file is removed.
pub fn write_ledger_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let target = resolve_target(path)?;
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "ledger path has no file name"))?;

    // The temp file is created exclusively (`create_new` refuses an existing
    // entry, symlinks included) under a name unique to this call, so nothing
    // pre-placed in the directory is followed and no two writers share one.
    let (mut file, temp) = loop {
        let temp = dir.join(format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp) {
            Ok(file) => break (file, temp),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };

    let result = fill_and_rename(&mut file, &target, &temp, contents);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn fill_and_rename(
    file: &mut std::fs::File,
    target: &Path,
    temp: &Path,
    contents: &str,
) -> std::io::Result<()> {
    // Permissions are settled before any byte is written, so the contents are
    // never readable by more than could read the original.
    let mode = match std::fs::metadata(target) {
        Ok(metadata) => metadata.permissions().mode() & 0o7777,
        Err(_) => NEW_LEDGER_MODE,
    };
    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temp, target)
}

/// The path the bytes must land on: the symlink's final target, or `path`.
fn resolve_target(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}
