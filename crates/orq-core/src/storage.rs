//! Where Orq keeps its own data: XDG roots outside any project tree, created
//! owner-only. Nothing here touches project or MCP-owned files.

use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

/// Set by orq's own test runs: storage then refuses roots outside the temp dir.
pub const TEST_SANDBOX: &str = "ORQ_TEST_SANDBOX";

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
        let roots = StorageRoots {
            data: pick("XDG_DATA_HOME", &[".local", "share"])?,
            config: pick("XDG_CONFIG_HOME", &[".config"])?,
            state: pick("XDG_STATE_HOME", &[".local", "state"])?,
            cache: pick("XDG_CACHE_HOME", &[".cache"])?,
        };
        if env(TEST_SANDBOX).as_deref() == Some("1") {
            roots.check_sandboxed(env)?;
        }
        Ok(roots)
    }

    /// Under `ORQ_TEST_SANDBOX=1` every root must be inside the temp dir (`TMPDIR`, else `/tmp`),
    /// so a test that forgot to move one fails instead of writing into the person's real folders.
    fn check_sandboxed(&self, env: &dyn Fn(&str) -> Option<String>) -> std::io::Result<()> {
        let temp = env("TMPDIR").map(PathBuf::from).filter(|dir| dir.is_absolute()).unwrap_or_else(|| PathBuf::from("/tmp"));
        let real_temp = real_path(&temp);
        for (var, root) in [("XDG_DATA_HOME", &self.data), ("XDG_CONFIG_HOME", &self.config), ("XDG_STATE_HOME", &self.state), ("XDG_CACHE_HOME", &self.cache)] {
            let climbs = root.components().any(|part| part == std::path::Component::ParentDir);
            // Compared where the path really leads, so a symlink inside the temp dir pointing at
            // the real folders does not pass.
            if climbs || !real_path(root).starts_with(&real_temp) {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    format!("{TEST_SANDBOX}=1 refuses the orq folder {}: it is not inside the temporary directory {}; point HOME or {var} at a temporary directory", root.display(), temp.display()),
                ));
            }
        }
        Ok(())
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

/// `path` with its deepest existing ancestor's symlinks resolved and the rest (not created yet)
/// appended unchanged; `path` itself when nothing of it can be resolved.
fn real_path(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
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
        // The child's entry lives in `dir`. Sync it whether this call created
        // the child or found it: a concurrent creator may not have synced yet,
        // and a crash must not lose the directory (and the report inside it)
        // after this write returned.
        fsync(&dir)?;
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
    write_atomic(data_root, &Path::new("runs").join(run_id).join(file.file_name()), &crate::redact::redact_bytes(bytes))
}

/// The newest ledger snapshots kept per project, however old.
const KEEP_NEWEST_LEDGER_SNAPSHOTS: usize = 200;
/// Beyond those, the newest snapshot of each UTC day is kept for this many days.
const KEEP_DAILY_LEDGER_DAYS: u64 = 60;
const SECONDS_PER_DAY: u64 = 86_400;

/// One saved copy of a project's ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerSnapshot {
    pub path: PathBuf,
    /// Seconds since the Unix epoch, from the file name.
    pub taken_at: u64,
    pub hash: String,
}

/// The first eight hex digits of the SHA-256 of `bytes`.
pub fn content_hash8(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// `YYYYMMDDTHHMMSSZ` for `secs` since the epoch.
pub fn utc_stamp(secs: u64) -> String {
    let (year, month, day) = civil_from_days((secs / SECONDS_PER_DAY) as i64);
    let rest = secs % SECONDS_PER_DAY;
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn stamp_secs(stamp: &str) -> Option<u64> {
    let digits = |range: std::ops::Range<usize>| stamp.get(range).filter(|s| s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| s.parse::<u64>().ok());
    if stamp.len() != 16 || stamp.as_bytes()[8] != b'T' || !stamp.ends_with('Z') {
        return None;
    }
    let days = days_from_civil(digits(0..4)? as i64, digits(4..6)? as u32, digits(6..8)? as u32);
    Some(u64::try_from(days).ok()? * SECONDS_PER_DAY + digits(9..11)? * 3600 + digits(11..13)? * 60 + digits(13..15)?)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Where a project's ledger snapshots live under the data root.
pub fn ledger_snapshot_dir(data_root: &Path, project_id: &str) -> PathBuf {
    data_root.join("snapshots").join(project_id)
}

/// A project's ledger snapshots, oldest first. A missing folder holds none; files that are not
/// named like a snapshot are left out.
pub fn list_ledger_snapshots(data_root: &Path, project_id: &str) -> Vec<LedgerSnapshot> {
    let Ok(entries) = std::fs::read_dir(ledger_snapshot_dir(data_root, project_id)) else { return Vec::new() };
    let mut found: Vec<LedgerSnapshot> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let middle = name.strip_prefix("ledger-")?.strip_suffix(".json")?;
            let (stamp, hash) = middle.split_once('-')?;
            Some(LedgerSnapshot { path: entry.path(), taken_at: stamp_secs(stamp)?, hash: hash.to_string() })
        })
        .collect();
    found.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
    found
}

/// Copies `bytes`, a ledger about to be replaced, to
/// `<data>/snapshots/<project id>/ledger-<UTC timestamp>-<8-char hash>.json`, privately and
/// atomically, then trims that project's older snapshots. Nothing is written when the newest
/// snapshot already holds the same bytes. Returns the new file.
#[cfg(unix)]
pub fn snapshot_ledger(data_root: &Path, project_id: &str, bytes: &[u8], now_secs: u64) -> std::io::Result<Option<PathBuf>> {
    let hash = content_hash8(bytes);
    if list_ledger_snapshots(data_root, project_id).last().is_some_and(|newest| newest.hash == hash) {
        return Ok(None);
    }
    create_private_dir(data_root)?;
    let rel = Path::new("snapshots").join(project_id).join(format!("ledger-{}-{hash}.json", utc_stamp(now_secs)));
    write_atomic(data_root, &rel, bytes)?;
    prune_ledger_snapshots(data_root, project_id, now_secs);
    Ok(Some(data_root.join(rel)))
}

/// Keeps the newest 200 snapshots and, of the older ones, the newest of each UTC day for 60 days.
fn prune_ledger_snapshots(data_root: &Path, project_id: &str, now_secs: u64) {
    let all = list_ledger_snapshots(data_root, project_id);
    let older = all.len().saturating_sub(KEEP_NEWEST_LEDGER_SNAPSHOTS);
    let oldest_day = (now_secs / SECONDS_PER_DAY).saturating_sub(KEEP_DAILY_LEDGER_DAYS);
    let mut kept_days = std::collections::BTreeSet::new();
    // Newest first, so the first one met on a day is that day's newest.
    for snapshot in all[..older].iter().rev() {
        let day = snapshot.taken_at / SECONDS_PER_DAY;
        if day < oldest_day || !kept_days.insert(day) {
            let _ = std::fs::remove_file(&snapshot.path);
        }
    }
}

/// The daily database snapshots kept.
const KEEP_DATABASE_SNAPSHOTS: usize = 14;
/// A database snapshot is taken again once the newest is older than this.
const DATABASE_SNAPSHOT_EVERY: u64 = SECONDS_PER_DAY;

/// The folder every snapshot lives in.
pub fn snapshots_dir(data_root: &Path) -> PathBuf {
    data_root.join("snapshots")
}

/// The daily database snapshots (`orq-db-<date>.sqlite`), oldest first, with the time each was
/// written.
pub fn list_database_snapshots(data_root: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(snapshots_dir(data_root)) else { return Vec::new() };
    let mut found: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let date = name.strip_prefix("orq-db-").and_then(|rest| rest.strip_suffix(".sqlite"));
            date.is_some_and(|d| d.len() == 10 && d.bytes().enumerate().all(|(i, b)| if matches!(i, 4 | 7) { b == b'-' } else { b.is_ascii_digit() }))
        })
        .map(|entry| {
            let written = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            (entry.path(), written)
        })
        .collect();
    found.sort();
    found
}

/// `20261120T101500Z`-style seconds as `2026-11-20 10:15:00Z`.
pub fn readable_utc(secs: u64) -> String {
    let s = utc_stamp(secs);
    format!("{}-{}-{} {}:{}:{}Z", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15])
}

/// What the snapshots take up, and a line for the database and for each project saying when its
/// newest was taken. `name_of` turns a project's snapshot folder name into what to show for it.
pub fn snapshot_summary(data_root: &Path, name_of: &dyn Fn(&str) -> Option<String>) -> (u64, Vec<String>) {
    let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let (mut bytes, mut lines) = (0u64, Vec::new());
    let databases = list_database_snapshots(data_root);
    if let Some((_, written)) = databases.last() {
        let total: u64 = databases.iter().map(|(path, _)| size(path)).sum();
        bytes += total;
        lines.push(format!("Database: newest {} ({} kept, {})", readable_utc(*written), databases.len(), crate::retention::human_bytes(total)));
    }
    let mut ids: Vec<String> = std::fs::read_dir(snapshots_dir(data_root))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    for id in ids {
        let ledgers = list_ledger_snapshots(data_root, &id);
        let Some(newest) = ledgers.last() else { continue };
        let total: u64 = ledgers.iter().map(|s| size(&s.path)).sum();
        bytes += total;
        let name = name_of(&id).unwrap_or(id);
        lines.push(format!("{name}: newest {} ({} kept, {})", readable_utc(newest.taken_at), ledgers.len(), crate::retention::human_bytes(total)));
    }
    (bytes, lines)
}

/// Whether the newest database snapshot is missing or more than a day old.
pub fn database_snapshot_due(data_root: &Path, now_secs: u64) -> bool {
    list_database_snapshots(data_root).last().is_none_or(|(_, written)| now_secs.saturating_sub(*written) > DATABASE_SNAPSHOT_EVERY)
}

/// Writes `<data>/snapshots/orq-db-<date>.sqlite` with SQLite's `VACUUM INTO` (a consistent copy even
/// while the database is in use), privately, then keeps the newest 14.
#[cfg(unix)]
pub fn snapshot_database(data_root: &Path, now_secs: u64) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let database = data_root.join("orq.db");
    let dir = snapshots_dir(data_root);
    create_private_dir(data_root)?;
    create_private_dir(&dir)?;
    let stamp = utc_stamp(now_secs);
    let target = dir.join(format!("orq-db-{}-{}-{}.sqlite", &stamp[0..4], &stamp[4..6], &stamp[6..8]));
    let temp = dir.join(format!(".orq-db-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let copy = || -> Result<(), Box<dyn std::error::Error>> {
        let connection = rusqlite::Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute("VACUUM INTO ?1", [temp.to_string_lossy().as_ref()])?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&temp, &target)?;
        Ok(())
    };
    if let Err(error) = copy() {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::other(error.to_string()));
    }
    let all = list_database_snapshots(data_root);
    for (old, _) in &all[..all.len().saturating_sub(KEEP_DATABASE_SNAPSHOTS)] {
        let _ = std::fs::remove_file(old);
    }
    Ok(target)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // Parents (such as ~/.local/share) may legitimately be symlinks; only
        // the Orq directory itself is checked.
        if let Some(parent) = dir.parent() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        // A directory that already existed may be a symlink planted elsewhere
        // or have loose permissions. Open it without following a symlink and
        // inspect and chmod that same descriptor, so a link swapped in after
        // the check can never have its target's permissions changed (fchmod
        // fails for a directory owned by someone else).
        let handle = rustix::fs::open(dir, rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC, rustix::fs::Mode::empty())
            .map_err(|error| Error::new(ErrorKind::PermissionDenied, format!("{} is not a plain directory: {error}", dir.display())))?;
        let handle = std::fs::File::from(handle);
        if handle.metadata()?.permissions().mode() & 0o777 != 0o700 {
            handle.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}
