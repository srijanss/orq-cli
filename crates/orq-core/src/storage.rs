//! Where Orq keeps its own data: XDG roots outside any project tree, created
//! owner-only. Nothing here touches project or MCP-owned files.

use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

/// Orq's four roots, each already ending in `orq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoots {
    pub data: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl StorageRoots {
    /// Resolves the roots through `env`, so callers (and tests) choose the
    /// environment. An XDG value that is empty or relative is ignored, as the
    /// XDG specification requires; without an absolute `HOME` there is no
    /// fallback and resolution fails rather than writing into the working directory.
    pub fn resolve(env: &dyn Fn(&str) -> Option<String>) -> std::io::Result<Self> {
        let home = env("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
        let pick = |var: &str, default: &[&str]| -> std::io::Result<PathBuf> {
            if let Some(base) = env(var).map(PathBuf::from).filter(|base| base.is_absolute()) {
                return Ok(base.join("orq"));
            }
            let home = home.as_ref().ok_or_else(|| Error::new(ErrorKind::NotFound, format!("{var} is unset and HOME is not an absolute path")))?;
            Ok(default.iter().fold(home.clone(), |path, part| path.join(part)).join("orq"))
        };
        Ok(StorageRoots {
            data: pick("XDG_DATA_HOME", &[".local", "share"])?,
            config: pick("XDG_CONFIG_HOME", &[".config"])?,
            state: pick("XDG_STATE_HOME", &[".local", "state"])?,
            cache: pick("XDG_CACHE_HOME", &[".cache"])?,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("orq.db")
    }

    pub fn runs(&self) -> PathBuf {
        self.data.join("runs")
    }

    pub fn logs(&self) -> PathBuf {
        self.state.join("logs")
    }

    /// Creates every directory owner-only (`0700`); safe to call repeatedly.
    pub fn create_all(&self) -> std::io::Result<()> {
        for dir in [&self.data, &self.config, &self.state, &self.cache, &self.runs(), &self.logs()] {
            create_private_dir(dir)?;
        }
        Ok(())
    }
}

static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `bytes` to `rel` (relative, no `..`) inside `root`. The data goes to
/// a private temporary file in the destination directory that is synced and
/// then renamed over the target, so a reader or a crash sees the old file or
/// the whole new one, never a partial write. Directories are created
/// owner-only and opened one at a time without following symlinks, so a
/// symlink planted inside the root cannot redirect the write; a symlink at the
/// target itself is replaced, never written through.
#[cfg(unix)]
pub fn write_atomic(root: &Path, rel: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use rustix::fs::{fsync, mkdirat, open, openat, renameat, unlinkat, AtFlags, Mode, OFlags};
    use rustix::io::Errno;
    use std::io::Write;
    use std::path::Component;

    let refuse = |why: &str| Error::new(ErrorKind::PermissionDenied, why.to_string());
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            _ => return Err(refuse("path must stay inside the Orq root")),
        }
    }
    let (name, dirs) = parts.split_last().ok_or_else(|| refuse("nothing to write"))?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = open(root, dir_flags, Mode::empty())?;
    for part in dirs {
        match mkdirat(&dir, *part, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        dir = openat(&dir, *part, dir_flags, Mode::empty())?;
    }
    let temp = format!(".{}.{}.{}.tmp", name.to_string_lossy(), std::process::id(), TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let fd = openat(&dir, temp.as_str(), OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o600))?;
    let mut file = std::fs::File::from(fd);
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let finished = written.and_then(|()| renameat(&dir, temp.as_str(), &dir, *name).map_err(Error::from));
    if let Err(error) = finished {
        let _ = unlinkat(&dir, temp.as_str(), AtFlags::empty());
        return Err(error);
    }
    fsync(&dir)?;
    Ok(())
}

/// Run ids become directory names, so only a plain, short word is accepted.
pub fn check_run_id(run_id: &str) -> std::io::Result<()> {
    let plain = !run_id.is_empty() && run_id.len() <= 128 && run_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain { Ok(()) } else { Err(Error::new(ErrorKind::InvalidInput, format!("run id {run_id:?} is not a plain identifier"))) }
}

/// Head SHAs become directory names, so only lowercase hex of a real length is accepted.
pub fn check_head_sha(head_sha: &str) -> std::io::Result<()> {
    let hex = (7..=64).contains(&head_sha.len()) && head_sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    if hex { Ok(()) } else { Err(Error::new(ErrorKind::InvalidInput, format!("head sha {head_sha:?} is not lowercase hex"))) }
}

/// The per-run files Orq writes besides review reports; each has one fixed name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFile {
    Tests,
    ReviewRounds,
    Handoff,
    Events,
}

impl RunFile {
    pub fn file_name(self) -> &'static str {
        match self {
            RunFile::Tests => "tests.json",
            RunFile::ReviewRounds => "review-rounds.json",
            RunFile::Handoff => "handoff.json",
            RunFile::Events => "events.jsonl",
        }
    }
}

/// Atomically writes one of a run's fixed files to `runs/<run_id>/` under `data_root`.
#[cfg(unix)]
pub fn save_run_file(data_root: &Path, run_id: &str, file: RunFile, bytes: &[u8]) -> std::io::Result<()> {
    check_run_id(run_id)?;
    write_atomic(data_root, &Path::new("runs").join(run_id).join(file.file_name()), bytes)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}
