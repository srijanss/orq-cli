//! The ignored files orq itself puts into a run's worktree, and whether they are still what orq wrote.
//! A fingerprint of each is recorded in the worktree's own git directory; cleanup and reap treat such a
//! file as orq's only while it still matches, so an edit made afterwards counts as the agent's work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::outcome::git_raw;

/// Ignored paths orq itself writes (or links) into a run's worktree.
const CANDIDATES: [&str; 5] = [".mcp.json", ".mcpctl.toml", ".mcpctl.lock", ".claude/settings.local.json", concat!(".", "env")];
const MANIFEST: &str = "orq-written.json";

/// What `path` is now: a link's target, or a regular file's content hash. `None` for anything else.
fn fingerprint(worktree: &Path, name: &str) -> Option<String> {
    let path = worktree.join(name);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        return Some(format!("link:{}", std::fs::read_link(&path).ok()?.display()));
    }
    if !meta.is_file() {
        return None;
    }
    let out = git_raw(worktree, &["hash-object", "--", name]).ok()?;
    out.success.then(|| format!("blob:{}", out.stdout.trim()))
}

fn manifest_path(worktree: &Path) -> Option<PathBuf> {
    let out = git_raw(worktree, &["rev-parse", "--absolute-git-dir"]).ok()?;
    out.success.then(|| PathBuf::from(out.stdout.trim()).join(MANIFEST))
}

/// Records what orq wrote into `worktree`, so cleanup and reap can tell it from the agent's own work.
/// Returns a note on a problem.
pub fn record_orq_written(worktree: &Path) -> Vec<String> {
    let Some(path) = manifest_path(worktree) else {
        return vec!["what orq wrote into the worktree was not recorded: its git directory could not be found".to_string()];
    };
    let recorded: BTreeMap<String, String> =
        CANDIDATES.iter().filter_map(|name| fingerprint(worktree, name).map(|print| (name.to_string(), print))).collect();
    match serde_json::to_string(&recorded).map_err(|e| e.to_string()).and_then(|text| std::fs::write(&path, text).map_err(|e| format!("{:?}", e.kind()))) {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("what orq wrote into the worktree was not recorded: {why}")],
    }
}

/// Whether `name` is something orq recorded writing and is still exactly that.
pub(crate) fn is_unchanged_orq_file(worktree: &Path, name: &str) -> bool {
    let Some(text) = manifest_path(worktree).and_then(|path| std::fs::read_to_string(path).ok()) else {
        return false;
    };
    let Ok(recorded) = serde_json::from_str::<BTreeMap<String, String>>(&text) else {
        return false;
    };
    recorded.get(name).is_some_and(|print| fingerprint(worktree, name).as_ref() == Some(print))
}
