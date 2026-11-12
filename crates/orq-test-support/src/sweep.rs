//! Publishing and sweeping the per-process temp roots.

use std::ffi::CString;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// What became of a staged root on its way to its published name.
pub(crate) enum Published {
    Done,
    /// Something is already at the name: try the next one.
    Taken,
    /// Anything else, which another name would not cure.
    Failed(std::io::Error),
}

impl Published {
    fn of(result: std::io::Result<()>) -> Published {
        match result {
            Ok(()) => Published::Done,
            Err(error) if matches!(error.kind(), ErrorKind::AlreadyExists | ErrorKind::DirectoryNotEmpty) => Published::Taken,
            Err(error) => Published::Failed(error),
        }
    }
}

/// Moves `staged` to `dest` only while nothing is at `dest`, in one step. A plain rename would
/// replace an empty directory another process just published there. Where the system or the
/// file system has no such rename, the name is claimed first instead.
pub(crate) fn publish(staged: &Path, dest: &Path) -> Published {
    match rename_no_replace(staged, dest) {
        Some(Err(error)) if matches!(error.kind(), ErrorKind::Unsupported | ErrorKind::InvalidInput) => publish_by_claiming_the_name(staged, dest),
        Some(result) => Published::of(result),
        None => publish_by_claiming_the_name(staged, dest),
    }
}

fn c_paths(staged: &Path, dest: &Path) -> std::io::Result<(CString, CString)> {
    let c = |path: &Path| CString::new(path.as_os_str().as_bytes()).map_err(|_| std::io::Error::new(ErrorKind::InvalidFilename, "a NUL byte in the path"));
    Ok((c(staged)?, c(dest)?))
}

/// The system's rename that fails rather than replace anything at `dest`; `None` where there is none.
#[cfg(target_os = "linux")]
fn rename_no_replace(staged: &Path, dest: &Path) -> Option<std::io::Result<()>> {
    extern "C" {
        fn renameat2(old_dir: i32, old: *const std::ffi::c_char, new_dir: i32, new: *const std::ffi::c_char, flags: u32) -> i32;
    }
    const AT_FDCWD: i32 = -100;
    const RENAME_NOREPLACE: u32 = 1;
    Some(c_paths(staged, dest).and_then(|(old, new)| {
        // SAFETY: both paths are NUL-terminated strings that outlive the call.
        match unsafe { renameat2(AT_FDCWD, old.as_ptr(), AT_FDCWD, new.as_ptr(), RENAME_NOREPLACE) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn rename_no_replace(staged: &Path, dest: &Path) -> Option<std::io::Result<()>> {
    extern "C" {
        fn renamex_np(old: *const std::ffi::c_char, new: *const std::ffi::c_char, flags: u32) -> i32;
    }
    const RENAME_EXCL: u32 = 0x4;
    Some(c_paths(staged, dest).and_then(|(old, new)| {
        // SAFETY: both paths are NUL-terminated strings that outlive the call.
        match unsafe { renamex_np(old.as_ptr(), new.as_ptr(), RENAME_EXCL) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
fn rename_no_replace(_staged: &Path, _dest: &Path) -> Option<std::io::Result<()>> {
    None
}

/// Publishes without a no-replace rename: the name is claimed with an exclusive mkdir, which only
/// one process can win, and the staged root is then renamed over that empty directory of its own.
/// The claimed directory is unlocked for that moment, so the no-replace rename is preferred.
pub(crate) fn publish_by_claiming_the_name(staged: &Path, dest: &Path) -> Published {
    use std::os::unix::fs::DirBuilderExt;
    if let claim @ (Published::Taken | Published::Failed(_)) = Published::of(std::fs::DirBuilder::new().mode(0o700).create(dest)) {
        return claim;
    }
    match std::fs::rename(staged, dest) {
        Ok(()) => Published::Done,
        Err(error) => {
            let _ = std::fs::remove_dir(dest);
            Published::Failed(error)
        }
    }
}

/// Whether the entry `name` is a root a sweep may remove: named `orq-tests-<pid>-<n>` with a
/// positive PID, a real directory owned by `me`, and its process `gone`. `metadata` answers
/// `(is_dir, owner)` without following symlinks, `None` when the entry is gone; it is asked only
/// for a root's name, so the unrelated entries of a shared temp dir cost no lookup.
pub(crate) fn sweepable(name: &str, metadata: impl FnOnce() -> Option<(bool, u32)>, me: u32, gone: impl FnOnce(i32) -> bool) -> bool {
    let Some((pid, unique)) = name.strip_prefix("orq-tests-").and_then(|rest| rest.split_once('-')) else { return false };
    let (Ok(pid), Ok(_)) = (pid.parse::<i32>(), unique.parse::<u64>()) else { return false };
    if pid <= 0 {
        return false;
    }
    let Some((is_dir, owner)) = metadata() else { return false };
    is_dir && owner == me && gone(pid)
}

/// Whether `kill(pid, 0)` returning `result` with `errno` means no such process exists. Only
/// "no such process" does: "not permitted" answers for another user's process, which exists.
pub(crate) fn gone_after_probe(result: i32, errno: Option<i32>) -> bool {
    const ESRCH: i32 = 3;
    result != 0 && errno == Some(ESRCH)
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod tests;
