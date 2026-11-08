//! Projects the server can show: the git repositories directly inside the scan root
//! (`~/code` by default). Symlinks are never followed, so nothing outside the root is read.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

/// The scan root: an explicit override (for tests), else `code` inside the home directory.
pub fn default_root(home: Option<&Path>, override_root: Option<&Path>) -> PathBuf {
    match (override_root, home) {
        (Some(root), _) => root.to_path_buf(),
        (None, Some(home)) => home.join("code"),
        (None, None) => PathBuf::from("code"),
    }
}

/// Scans the root now. A symlinked root is allowed on purpose and is resolved once; every project
/// is then checked to still resolve to a direct child of that real root, so a folder swapped for a
/// link to somewhere else is dropped. A missing or unreadable root, or entry, gives no project.
pub fn discover(root: &Path) -> Vec<Project> {
    let Ok(real_root) = std::fs::canonicalize(root) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    collect(&real_root, "", GROUP_DEPTH, &mut found);
    found.sort();
    found.into_iter().map(|(name, path)| Project { id: project_id(&name), name, path }).collect()
}

/// What a run's event log said when it was last read, and the size and time it had then.
struct LogSummary {
    len: u64,
    modified: Option<std::time::SystemTime>,
    state: &'static str,
    stage: Option<String>,
    started: Option<String>,
    updated: Option<String>,
}

type LogCache = std::collections::HashMap<String, LogSummary>;

/// One listed run: its sort time, id and JSON row. `None` when the run belongs to another project
/// or was removed after its record was read; an unreadable event log is an error. A log whose size
/// and time are what they were is not read again (the log only ever grows), so polling the list
/// costs a stat per run, not a replay of its history.
fn run_row(store: &orq_orchestrator::RunStore, state: &Path, cache: &mut LogCache, record: orq_orchestrator::RunRecord, key: &str) -> Result<Option<(Option<u64>, String, serde_json::Value)>, String> {
    use orq_orchestrator::StoreError;
    if record.project_id != key {
        return Ok(None);
    }
    let stat = std::fs::metadata(state.join("runs").join(&record.run_id).join("events.jsonl")).ok().map(|meta| (meta.len(), meta.modified().ok()));
    let fresh = stat.is_some_and(|(len, modified)| cache.get(&record.run_id).is_some_and(|known| known.len == len && known.modified == modified && modified.is_some()));
    if !fresh {
        let events = match store.events(&record.run_id) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => {
                cache.remove(&record.run_id);
                return Ok(None);
            }
            Err(error) => return Err(format!("run {} is unreadable: {error:?}", record.run_id)),
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let learned = LogSummary {
            len: stat.map_or(0, |(len, _)| len),
            modified: stat.and_then(|(_, modified)| modified),
            state: summary.phase.as_str(),
            stage: summary.stage,
            started: events.first().map(|event| event.occurred_at.clone()),
            updated: events.last().map(|event| event.occurred_at.clone()),
        };
        if stat.is_some() {
            cache.insert(record.run_id.clone(), learned);
        } else {
            cache.remove(&record.run_id);
            return Ok(Some(row_of(&record, &learned)));
        }
    }
    Ok(Some(row_of(&record, &cache[&record.run_id])))
}

fn row_of(record: &orq_orchestrator::RunRecord, log: &LogSummary) -> (Option<u64>, String, serde_json::Value) {
    let sort_key = log.updated.as_deref().and_then(orq_orchestrator::parse_rfc3339);
    let row = serde_json::json!({"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": log.state, "stage": log.stage, "started_at": log.started, "updated_at": log.updated});
    (sort_key, record.run_id.clone(), row)
}

/// How many events one timeline answer holds; `next_cursor` continues after the last of them.
const TIMELINE_PAGE: usize = 200;
/// Lines in one page of a log, and the most of a log file that is read (and so shown).
const LOG_PAGE: usize = 500;
const LOG_READ_CAP: u64 = 8 * 1024 * 1024;

/// How many plain folders may be nested above a repository: `~/code/pitstop/eGiftit/egiftit-backend`
/// has two, and nothing deeper is looked at.
const GROUP_DEPTH: usize = 2;

/// Adds the repositories inside `dir`; a plain folder is looked into while `groups` allows it.
fn collect(dir: &Path, prefix: &str, groups: usize, found: &mut Vec<(String, PathBuf)>) {
    for name in child_folders(dir) {
        let path = dir.join(&name);
        let shown = format!("{prefix}{name}");
        if is_repository(&path) {
            found.push((shown, path));
        } else if groups > 0 {
            collect(&path, &format!("{shown}/"), groups - 1, found);
        }
    }
}

/// The names of the real directories directly inside `dir`: a link, a file or an unreadable
/// entry is left out, and so is a folder that no longer resolves to itself.
fn child_folders(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = dir.join(&name);
            (std::fs::canonicalize(&path).ok()? == path).then_some(name)
        })
        .collect()
}

/// A `.git` directory, or a `.git` file as linked worktrees have, that is not itself a symlink.
fn is_repository(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).map(|meta| !meta.file_type().is_symlink()).unwrap_or(false)
}

fn slug(name: &str) -> String {
    let cleaned: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let parts: Vec<&str> = cleaned.split('-').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() { "project".to_string() } else { parts.join("-") }
}

/// The id comes from the folder name alone, so it never changes when other folders come or go.
/// A name that is already a plain lowercase slug is its own id. Any other name gets its readable
/// slug plus `_` and the hex of the whole name; a plain slug never contains `_`, so the two
/// kinds cannot meet and two different names can never share an id.
fn project_id(name: &str) -> String {
    let readable = slug(name);
    if readable == name && name.len() <= MAX_ID_LEN {
        return readable;
    }
    let hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
    if readable.len() + 1 + hex.len() <= MAX_ID_LEN {
        return format!("{readable}_{hex}");
    }
    // A long name would overflow what the page accepts: keep a short readable start and a fixed
    // 128-bit digest instead. The `__` never occurs in the other two kinds, so they cannot meet.
    let start: String = readable.chars().take(40).collect();
    format!("{}__{:016x}{:016x}", start.trim_end_matches('-'), fnv1a(name, 0xcbf2_9ce4_8422_2325), fnv1a(name, 0x8422_2325_cbf2_9ce4))
}

/// The longest id the page accepts is 128 characters; stay well under it.
const MAX_ID_LEN: usize = 100;

/// FNV-1a: a stable hash that never changes between releases, unlike the standard hasher.
fn fnv1a(text: &str, seed: u64) -> u64 {
    text.bytes().fold(seed, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The projects provider the server hands to the handler: it rescans the scan root on each
/// request and reads counts from the same storage the CLI writes. A project it cannot read is
/// listed as unavailable, never dropped.
pub struct StoreProjects {
    roots: orq_core::storage::StorageRoots,
    scan_root: std::path::PathBuf,
    run_logs: std::sync::Mutex<LogCache>,
    log_snapshot: std::sync::Mutex<Option<LogSnapshot>>,
}

/// The redacted text of the log file last paged through, so the next page of an unchanged file is
/// cut from it instead of reading and redacting the file again.
struct LogSnapshot {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    truncated: bool,
    text: std::sync::Arc<str>,
}

impl StoreProjects {
    pub fn new(roots: orq_core::storage::StorageRoots, scan_root: std::path::PathBuf) -> Self {
        StoreProjects { roots, scan_root, run_logs: Default::default(), log_snapshot: Default::default() }
    }
}

/// Failures older than this are not counted as needing attention; the API states it with the count.
pub const FAILED_WINDOW_HOURS: u64 = 24;

#[derive(Clone, Copy, Default)]
struct RunCounts {
    active: usize,
    failed: usize,
}

struct Counts {
    features: usize,
    active_runs: usize,
    failed_runs: usize,
    reviews_waiting: usize,
}

impl orq_core::remote_inbox::ProjectsProvider for StoreProjects {
    fn review_scope(&self, id: &str) -> Result<Option<orq_core::remote_inbox::ReviewScope>, String> {
        // Only a project found by the scan counts; the id is matched, never used as a path.
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        Ok(Some(orq_core::remote_inbox::ReviewScope { review_key: orq_git::project_id(&git), name: project.name }))
    }

    fn features(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let list = read_ledger(&orq_features::ledger_path(&git))?
            .into_iter()
            .map(|entry| serde_json::json!({"name": entry.feature_name, "description": entry.description, "status": entry.status.as_str(), "depends_on": entry.depends_on, "recorded_at": entry.recorded_at}))
            .collect();
        Ok(Some(serde_json::Value::Array(list)))
    }

    fn runs(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let mut rows = Vec::new();
        // A poisoned lock only means an earlier request panicked; what it holds is still valid to re-check.
        let mut cache = self.run_logs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let run_ids = store.run_ids().map_err(|error| format!("{error:?}"))?;
        // Runs that are gone are forgotten, so the cache never outgrows the store.
        let live: std::collections::HashSet<&String> = run_ids.iter().collect();
        cache.retain(|id, _| live.contains(id));
        for run_id in run_ids {
            let record = match store.load(&run_id) {
                Ok(record) => record,
                Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
                Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
            };
            if let Some(row) = run_row(&store, &self.roots.state, &mut cache, record, &key)? {
                rows.push(row);
            }
        }
        // Newest first; a run with no readable time goes last, ties by run id.
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.cmp(&x).then_with(|| a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        });
        Ok(Some(serde_json::Value::Array(rows.into_iter().map(|row| row.2).collect())))
    }

    fn run_logs(&self, id: &str, run: &str, file: Option<&str>, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use std::io::Read;
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let logs_root = self.roots.state.join("logs");
        let dir = logs_root.join(run);
        // A link in place of either directory would lead outside the run's own logs.
        let real_dir = |path: &std::path::Path| match path.symlink_metadata() {
            Ok(meta) => Some(meta.is_dir()),
            Err(_) => None,
        };
        if real_dir(&logs_root) == Some(false) || real_dir(&dir) == Some(false) {
            return Ok(None);
        }
        // Only plain, non-hidden `.log` files that are not links are logs.
        let plain = |name: &str| name.ends_with(".log") && !name.starts_with('.') && !name.contains(['/', '\\']);
        let Some(file) = file else {
            let mut files = Vec::new();
            // A run that has written no logs has no directory; any other failure is an error.
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(|error| format!("logs of {run} are unreadable: {error}"))?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let meta = entry.path().symlink_metadata().map_err(|error| format!("log {name} is unreadable: {error}"))?;
                        if meta.is_file() && plain(&name) {
                            files.push(serde_json::json!({"name": name, "bytes": meta.len()}));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("logs of {run} are unreadable: {error}")),
            }
            files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            return Ok(Some(serde_json::json!({"files": files})));
        };
        if !plain(file) {
            return Ok(None);
        }
        let skip: usize = match after {
            None => 0,
            Some(cursor) => match cursor.parse() {
                Ok(lines) => lines,
                Err(_) => return Ok(None),
            },
        };
        let path = dir.join(file);
        match path.symlink_metadata() {
            Ok(meta) if meta.is_file() => {}
            _ => return Ok(None),
        }
        let meta = path.symlink_metadata().map_err(|error| format!("log {file} is unreadable: {error}"))?;
        let (len, modified) = (meta.len(), meta.modified().ok());
        let mut slot = self.log_snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = match slot.as_ref().filter(|known| after.is_some() && known.path == path && known.len == len && known.modified == modified && modified.is_some()) {
            Some(known) => (known.text.clone(), known.truncated),
            None => {
                let mut bytes = Vec::new();
                std::fs::File::open(&path).and_then(|f| f.take(LOG_READ_CAP + 1).read_to_end(&mut bytes)).map_err(|error| format!("log {file} is unreadable: {error}"))?;
                let truncated = bytes.len() as u64 > LOG_READ_CAP;
                bytes.truncate(LOG_READ_CAP as usize);
                // The whole readable text is redacted at once so a secret that spans lines is found.
                let text: std::sync::Arc<str> = String::from_utf8_lossy(&orq_core::redact::redact_bytes(&bytes)).into_owned().into();
                *slot = Some(LogSnapshot { path: path.clone(), len, modified, truncated, text: text.clone() });
                (text, truncated)
            }
        };
        drop(slot);
        let (text, truncated) = snapshot;
        let total = text.lines().count();
        if skip > total {
            return Ok(None);
        }
        let page: Vec<&str> = text.lines().skip(skip).take(LOG_PAGE).collect();
        let next = if skip + page.len() < total { Some((skip + page.len()).to_string()) } else { None };
        Ok(Some(serde_json::json!({"name": file, "lines": page, "next_cursor": next, "truncated": truncated})))
    }

    fn run_timeline(&self, id: &str, run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let events = match store.events(run) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        let start = match after {
            None => 0,
            Some(cursor) => match events.iter().position(|event| event.event_id == cursor) {
                Some(at) => at + 1,
                None => return Ok(None),
            },
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let page: Vec<_> = events.iter().skip(start).take(TIMELINE_PAGE).collect();
        let more = start + page.len() < events.len();
        let next_cursor = if more { page.last().map(|event| event.event_id.clone()) } else { None };
        // The payload is left out: it can hold reasons and paths the page has no need of.
        let listed: Vec<_> = page
            .iter()
            .map(|event| serde_json::json!({"event_id": event.event_id, "kind": event.kind, "stage": event.stage, "attempt": event.attempt, "occurred_at": event.occurred_at}))
            .collect();
        Ok(Some(serde_json::json!({
            "run": {"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": summary.phase.as_str(), "stage": summary.stage, "started_at": events.first().map(|event| &event.occurred_at), "updated_at": events.last().map(|event| &event.occurred_at)},
            "events": listed,
            "next_cursor": next_cursor,
        })))
    }

    fn projects(&self) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let reviews = if self.roots.database().exists() {
            Some(orq_core::store::Store::open_read_only(&self.roots.database()).map_err(|error| format!("cannot open the review index: {error}"))?)
        } else {
            None
        };
        let active = active_runs_by_project(&orq_orchestrator::RunStore::new(&self.roots.state));
        let list = discover(&self.scan_root)
            .into_iter()
            .map(|project| match counts(&project, reviews.as_ref(), &active) {
                Ok(counts) => json!({"id": project.id, "name": project.name, "available": true, "features": counts.features, "active_runs": counts.active_runs, "reviews_waiting": counts.reviews_waiting, "failed_runs": counts.failed_runs, "failed_window_hours": FAILED_WINDOW_HOURS}),
                Err(_) => json!({"id": project.id, "name": project.name, "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": FAILED_WINDOW_HOURS}),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::Value::Array(list))
    }
}

fn counts(project: &Project, reviews: Option<&orq_core::store::Store>, active: &Result<std::collections::HashMap<String, RunCounts>, String>) -> Result<Counts, String> {
    let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
    let key = orq_git::project_id(&git);
    let features = ledger_entries(&orq_features::ledger_path(&git))?;
    let runs = active.as_ref().map_err(Clone::clone)?.get(&key).copied().unwrap_or_default();
    let reviews_waiting = match reviews {
        Some(store) => waiting_reviews(store, &key)?,
        None => 0,
    };
    Ok(Counts { features, active_runs: runs.active, failed_runs: runs.failed, reviews_waiting })
}

/// The runs of `project` whose latest report is still waiting on a human: no lifecycle recorded for
/// that report's head yet, or one that is neither merged, closed nor resolved. A lifecycle that
/// cannot be read is an error, never a guess.
fn waiting_reviews(store: &orq_core::store::Store, project: &str) -> Result<usize, String> {
    use orq_core::review_lifecycle::{Gate, ProviderState};
    // Records come oldest first, so a later one for the same run replaces the earlier.
    let mut latest: std::collections::HashMap<String, orq_core::store::ReviewRecord> = std::collections::HashMap::new();
    for record in store.reviews_for_project(project).map_err(|error| error.to_string())? {
        latest.insert(record.run_id.clone(), record);
    }
    let mut waiting = 0;
    for record in latest.values() {
        let lifecycles = store.lifecycles_for_run(&record.run_id).map_err(|error| error.to_string())?;
        let finished = lifecycles
            .iter()
            .rev()
            .find(|lifecycle| lifecycle.reported_head_sha == record.head_sha)
            .is_some_and(|lifecycle| matches!(lifecycle.state, ProviderState::Merged | ProviderState::Closed) || lifecycle.gate == Gate::Resolved);
        if !finished {
            waiting += 1;
        }
    }
    Ok(waiting)
}

/// Active runs per project id, read in one pass over the run store. A record or event log that
/// cannot be read has no known owner, so no project's count can be trusted and this is an error.
fn active_runs_by_project(runs: &orq_orchestrator::RunStore) -> Result<std::collections::HashMap<String, RunCounts>, String> {
    use orq_orchestrator::{RunPhase, StoreError};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut counts = std::collections::HashMap::new();
    for run_id in runs.run_ids().map_err(|error| format!("{error:?}"))? {
        let record = match runs.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
        };
        let events = runs.events(&run_id).map_err(|error| format!("run {run_id} is unreadable: {error:?}"))?;
        let entry: &mut RunCounts = counts.entry(record.project_id).or_default();
        let summary = orq_orchestrator::summarize_run(&events);
        match summary.phase {
            RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused => entry.active += 1,
            RunPhase::Failed if failed_recently(summary.phase_at.as_deref(), now) => entry.failed += 1,
            _ => {}
        }
    }
    Ok(counts)
}

/// Whether the event that failed the run is no older than the window. A failure with no readable
/// time, or one from the future, is not counted.
fn failed_recently(failed_at: Option<&str>, now: u64) -> bool {
    let Some(at) = failed_at.and_then(orq_orchestrator::parse_rfc3339) else { return false };
    at <= now && now - at <= FAILED_WINDOW_HOURS * 3600
}

/// The most a feature ledger may weigh before the server refuses to read it.
const LEDGER_LIMIT: u64 = 1024 * 1024;

/// How many features the ledger at `path` lists; none when it does not exist. The file is opened
/// without following a link and without waiting on a pipe, must be a regular file, and is read
/// only up to a limit, so a ledger planted in a repository cannot reach elsewhere or stall a request.
fn ledger_entries(path: &Path) -> Result<usize, String> {
    read_ledger(path).map(|entries| entries.len())
}

/// The ledger's entries; a missing ledger has none. It is opened without following a link and
/// read only up to a limit, so a hostile file cannot stall or exhaust the server.
fn read_ledger(path: &Path) -> Result<Vec<orq_features::FeatureEntry>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("the ledger cannot be opened: {:?}", error.kind())),
    };
    let meta = file.metadata().map_err(|error| format!("the ledger cannot be inspected: {:?}", error.kind()))?;
    if !meta.is_file() {
        return Err("the ledger is not a regular file".to_string());
    }
    if meta.len() > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    let mut bytes = Vec::new();
    file.take(LEDGER_LIMIT + 1).read_to_end(&mut bytes).map_err(|error| format!("the ledger cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    let text = String::from_utf8(bytes).map_err(|_| "the ledger is not text".to_string())?;
    orq_features::parse_ledger(&text).map_err(|error| format!("the ledger is malformed: {error}"))
}

#[cfg(test)]
mod run_row_tests {
    use super::run_row;
    use orq_orchestrator::{RunRecord, RunStore};

    #[test]
    fn run_row_skips_a_run_removed_after_its_record_was_read() {
        let dir = std::env::temp_dir().join(format!("orq-run-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = RunStore::new(&dir);
        let record = RunRecord { run_id: "gone".into(), project_id: "p".into(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: "b".into(), worktree: dir.join("w"), repository: String::new(), profiles: Default::default() };
        store.create(&record).unwrap();
        let loaded = store.load("gone").unwrap();
        // Cleanup removes the run between the two reads.
        std::fs::remove_dir_all(dir.join("runs")).ok();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
        assert!(matches!(run_row(&store, &dir, &mut Default::default(), loaded, "p"), Ok(None)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
