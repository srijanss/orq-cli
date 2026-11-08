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
    for name in child_folders(&real_root) {
        let path = real_root.join(&name);
        if is_repository(&path) {
            found.push((name, path));
        } else {
            // A plain folder groups repositories (`~/code/pitstop/payment_hub`): one level, no deeper.
            for inner in child_folders(&path) {
                let inner_path = path.join(&inner);
                if is_repository(&inner_path) {
                    found.push((format!("{name}/{inner}"), inner_path));
                }
            }
        }
    }
    found.sort();
    found.into_iter().map(|(name, path)| Project { id: project_id(&name), name, path }).collect()
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
    if readable == name {
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
}

impl StoreProjects {
    pub fn new(roots: orq_core::storage::StorageRoots, scan_root: std::path::PathBuf) -> Self {
        StoreProjects { roots, scan_root }
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
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
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
    orq_features::parse_ledger(&text).map(|entries| entries.len()).map_err(|error| format!("the ledger is malformed: {error}"))
}
