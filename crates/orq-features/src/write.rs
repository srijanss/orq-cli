use std::io::Write;
use std::path::{Path, PathBuf};

/// Replaces the ledger at `path` with `contents` in one step: the bytes go to a
/// temp file in the same directory, which is then renamed over the original,
/// so a reader sees the old file or the new one, never a partial write.
///
/// The original's permission bits carry over. A symlinked ledger is written
/// through to its target, so the link itself stays a link. On any failure the
/// original is untouched and the temp file is removed.
pub fn write_ledger_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let target = resolve_target(path)?;
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "ledger path has no file name"))?;
    let temp = dir.join(format!(".{}.tmp-{}", file_name.to_string_lossy(), std::process::id()));

    let result = write_and_rename(&target, &temp, contents);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn write_and_rename(target: &Path, temp: &Path, contents: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(temp)?;
    file.write_all(contents.as_bytes())?;
    if let Ok(metadata) = std::fs::metadata(target) {
        file.set_permissions(metadata.permissions())?;
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, target)
}

/// The path the bytes must land on: the symlink's final target, or `path`.
fn resolve_target(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}
