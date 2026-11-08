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
    let Ok(entries) = std::fs::read_dir(&real_root) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = real_root.join(&name);
            let resolved = std::fs::canonicalize(&path).ok()?;
            (resolved == path && is_repository(&path)).then_some((name, path))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(name, path)| Project { id: project_id(&name), name, path }).collect()
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
    format!("{readable}_{hex}")
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

struct Counts {
    features: usize,
    active_runs: usize,
    reviews_waiting: usize,
}

impl orq_core::remote_inbox::ProjectsProvider for StoreProjects {
    fn projects(&self) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let reviews = if self.roots.database().exists() {
            Some(orq_core::store::Store::open_read_only(&self.roots.database()).map_err(|error| format!("cannot open the review index: {error}"))?)
        } else {
            None
        };
        let runs = orq_orchestrator::RunStore::new(&self.roots.state);
        let list = discover(&self.scan_root)
            .into_iter()
            .map(|project| match counts(&project, reviews.as_ref(), &runs) {
                Ok(counts) => json!({"id": project.id, "name": project.name, "available": true, "features": counts.features, "active_runs": counts.active_runs, "reviews_waiting": counts.reviews_waiting}),
                Err(_) => json!({"id": project.id, "name": project.name, "available": false, "features": null, "active_runs": null, "reviews_waiting": null}),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::Value::Array(list))
    }
}

fn counts(project: &Project, reviews: Option<&orq_core::store::Store>, runs: &orq_orchestrator::RunStore) -> Result<Counts, String> {
    let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
    let key = orq_git::project_id(&git);
    let features = match orq_features::load_ledger(&orq_features::ledger_path(&git)) {
        Ok(entries) => entries.len(),
        Err(orq_features::LedgerError::Missing { .. }) => 0,
        Err(error) => return Err(format!("{error:?}")),
    };
    let listed = orq_orchestrator::list_runs_report(runs, &key).map_err(|error| error.to_string())?;
    // A record that cannot be read has no known owner, so no project's count can be trusted.
    if listed.unreadable > 0 {
        return Err(format!("{} run records are unreadable", listed.unreadable));
    }
    let active_runs = listed
        .runs
        .iter()
        .filter(|run| matches!(run.phase, orq_orchestrator::RunPhase::Running | orq_orchestrator::RunPhase::PauseRequested | orq_orchestrator::RunPhase::Paused))
        .count();
    let reviews_waiting = match reviews {
        Some(store) => {
            let mut seen = std::collections::HashSet::new();
            store.reviews_for_project(&key).map_err(|error| error.to_string())?.into_iter().filter(|record| seen.insert(record.run_id.clone())).count()
        }
        None => 0,
    };
    Ok(Counts { features, active_runs, reviews_waiting })
}
