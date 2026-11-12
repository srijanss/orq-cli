//! Publishing and sweeping the per-process temp roots.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

extern "C" {
    fn renameat2(old_dir: i32, old: *const std::ffi::c_char, new_dir: i32, new: *const std::ffi::c_char, flags: u32) -> i32;
}

/// Moves `staged` to `dest` only while nothing is at `dest`, in one step; `false` when something
/// is. A plain rename would replace an empty directory another process just published there.
pub(crate) fn publish(staged: &Path, dest: &Path) -> bool {
    const AT_FDCWD: i32 = -100;
    const RENAME_NOREPLACE: u32 = 1;
    let (Ok(old), Ok(new)) = (CString::new(staged.as_os_str().as_bytes()), CString::new(dest.as_os_str().as_bytes())) else { return false };
    // SAFETY: both paths are NUL-terminated strings that outlive the call.
    unsafe { renameat2(AT_FDCWD, old.as_ptr(), AT_FDCWD, new.as_ptr(), RENAME_NOREPLACE) == 0 }
}

/// Whether the entry `name` is a root a sweep may remove: named `orq-tests-<pid>-<n>` with a
/// positive PID, a real directory (`is_dir` from metadata that does not follow symlinks), owned by
/// `me`, and its process `gone`. `gone` is asked last, only for such a root.
pub(crate) fn sweepable(name: &str, is_dir: bool, owner: u32, me: u32, gone: impl FnOnce(i32) -> bool) -> bool {
    let Some((pid, unique)) = name.strip_prefix("orq-tests-").and_then(|rest| rest.split_once('-')) else { return false };
    let (Ok(pid), Ok(_)) = (pid.parse::<i32>(), unique.parse::<u64>()) else { return false };
    pid > 0 && is_dir && owner == me && gone(pid)
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
