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

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod tests;
