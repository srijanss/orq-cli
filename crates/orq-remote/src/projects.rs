//! Projects the server can show: the git repositories directly inside the scan root
//! (`~/code` by default). Symlinks are never followed, so nothing outside the root is read.

use std::collections::HashMap;
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

/// Scans the root now. A missing or unreadable root, or an unreadable entry, gives no project.
pub fn discover(root: &Path) -> Vec<Project> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| is_repository(&entry.path()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    found.sort();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (name, _) in &found {
        *seen.entry(slug(name)).or_default() += 1;
    }
    found
        .into_iter()
        .map(|(name, path)| {
            let base = slug(&name);
            let id = if seen[&base] > 1 { format!("{base}-{:06x}", fingerprint(&name) & 0xff_ffff) } else { base };
            Project { id, name, path }
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

fn fingerprint(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
}
