//! The run's logs a fix or explain session reads, copied into its worktree so the session needs
//! nothing outside it. The copies are kept out of git and removed once the session ends; the
//! originals in the run's log folder never change.

use std::fs;
use std::path::{Path, PathBuf};

use crate::gate_fix::{clear_report, read_report, UNRELATED_FAILURE_FILE};
use crate::local_files::{crosses_symlink, exclude_locally};

/// Where in the worktree a session finds its copies.
pub const SESSION_FILES_DIR: &str = ".claude/orq-run";

/// Replace `<worktree>/.claude/orq-run/` with fresh copies of `files`, each under its own name, so
/// the session sees only this attempt's files; git never sees them. Returns the folder.
pub fn copy_session_files(worktree: &Path, files: &[&Path]) -> Result<PathBuf, String> {
    let dir = session_dir(worktree)?;
    exclude_locally(worktree, SESSION_FILES_DIR).map_err(|why| format!("cannot keep {SESSION_FILES_DIR} out of git: {why}"))?;
    remove(&dir)?;
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    for file in files {
        let Some(name) = file.file_name() else { continue };
        if let Err(err) = fs::copy(file, dir.join(name)) {
            // A session never starts from part of its files.
            let _ = remove(&dir);
            return Err(format!("cannot copy {} into the worktree: {err}", file.display()));
        }
    }
    Ok(dir)
}

/// Once the session has ended: keep the unrelated-failure report it wrote in its copies next to the
/// run's logs in `log_dir`, where orq reads it, then remove the copies.
pub fn finish_session_files(worktree: &Path, log_dir: &Path) -> Result<(), String> {
    let dir = session_dir(worktree)?;
    // The session may have put a link in the folder's place: what it points at is not its report.
    if is_link(&dir) {
        remove(&dir)?;
        return Err(format!("{} in {} was replaced by a link, so its report is not read", SESSION_FILES_DIR, worktree.display()));
    }
    let kept = match read_report(&dir.join(UNRELATED_FAILURE_FILE)) {
        Some(summary) => keep_report(&log_dir.join(UNRELATED_FAILURE_FILE), &summary),
        None => Ok(()),
    };
    // The copies go even when the report could not be kept.
    let removed = remove(&dir);
    kept.and(removed)
}

/// Write `summary` as the report at `path`, never through whatever is in its place.
fn keep_report(path: &Path, summary: &str) -> Result<(), String> {
    use std::io::Write;
    let failed = |err: std::io::Error| format!("cannot keep the unrelated-failure report in {}: {err}", path.display());
    clear_report(path).map_err(failed)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(failed)?;
    file.write_all(format!("{summary}\n").as_bytes()).map_err(failed)
}

/// Remove the copies from `worktree`, if there are any.
pub fn remove_session_files(worktree: &Path) -> Result<(), String> {
    remove(&session_dir(worktree)?)
}

/// The copies' folder in `worktree`. A `.claude` that is a link (the repository can hold one) would
/// carry the copies, and their removal, out of the worktree, so it is refused.
fn session_dir(worktree: &Path) -> Result<PathBuf, String> {
    if crosses_symlink(worktree, SESSION_FILES_DIR) {
        return Err(format!("{} in {} is a link, so no session files are kept there", SESSION_FILES_DIR, worktree.display()));
    }
    Ok(worktree.join(SESSION_FILES_DIR))
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Remove `dir`, if anything is there; a link in its place goes, never what it points at.
fn remove(dir: &Path) -> Result<(), String> {
    let removed = if is_link(dir) { fs::remove_file(dir) } else { fs::remove_dir_all(dir) };
    match removed {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {err}", dir.display())),
        _ => Ok(()),
    }
}
