//! The limits a project's settings put on starting a run: how many of its runs may be active at
//! once, and how much free disk the filesystem holding its worktrees must have. Each refusal says
//! what was found and what to do about it.

use std::io;
use std::path::{Path, PathBuf};

/// The free space on a filesystem, and where it is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeDisk {
    /// Bytes an unprivileged process may still write.
    pub bytes: u64,
    pub mount: PathBuf,
}

/// Overrides the free bytes [`free_disk`] reports, so tests can stand in a full disk. Only a debug
/// build reads it: a release build always measures.
const FREE_DISK_OVERRIDE: &str = "ORQ_TEST_FREE_DISK_BYTES";

const BYTES_PER_GB: u64 = 1_000_000_000;

/// The free space on the filesystem holding `path`, and that filesystem's mount point: the
/// highest ancestor of `path` still on its device.
pub fn free_disk(path: &Path) -> io::Result<FreeDisk> {
    let path = std::fs::canonicalize(path)?;
    let mount = mount_point(&path)?;
    let bytes = match overridden_free_bytes() {
        Some(bytes) => bytes,
        None => available_bytes(&path)?,
    };
    Ok(FreeDisk { bytes, mount })
}

fn overridden_free_bytes() -> Option<u64> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(FREE_DISK_OVERRIDE).ok()?.trim().parse().ok()
}

fn mount_point(path: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let device = std::fs::metadata(path)?.dev();
    let mut mount = path;
    while let Some(parent) = mount.parent() {
        if std::fs::metadata(parent)?.dev() != device {
            break;
        }
        mount = parent;
    }
    Ok(mount.to_path_buf())
}

fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte"))?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a NUL-terminated string and `stats` a writable statvfs.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

/// Why `project` may not start another run with `active` (run id, feature id) runs active under
/// a limit of `max_active`; `None` when it may.
pub fn limit_refusal(project: &str, max_active: Option<u8>, active: &[(String, String)]) -> Option<String> {
    let max_active = usize::from(max_active?);
    if active.len() < max_active {
        return None;
    }
    let runs = active.iter().map(|(run, feature)| format!("{run}, {feature}")).collect::<Vec<_>>().join("; ");
    let (noun, wait) = if active.len() == 1 { ("run", "it") } else { ("runs", "one") };
    Some(format!(
        "{project} already has {} active {noun} ({runs}): wait for {wait} to finish, or raise the limit in project settings",
        active.len()
    ))
}

/// Why `project` may not start a run with `free` space under a floor of `floor_gb` gigabytes;
/// `None` when it may.
pub fn disk_refusal(project: &str, floor_gb: Option<u32>, free: &FreeDisk) -> Option<String> {
    let floor_gb = floor_gb?;
    if free.bytes >= u64::from(floor_gb) * BYTES_PER_GB {
        return None;
    }
    let free_gb = free.bytes as f64 / BYTES_PER_GB as f64;
    // Rounded down, so a disk just short of the floor never reads as having it.
    let shown = (free_gb * 10.0).floor() / 10.0;
    Some(format!(
        "only {shown:.1} GB free on {} ({project} needs {floor_gb} GB to start a run): free some space, or lower the floor in project settings",
        free.mount.display()
    ))
}
