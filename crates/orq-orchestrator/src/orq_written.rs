//! The ignored files orq itself puts into a run's worktree, and whether they are still what orq wrote.
//! A fingerprint of each is recorded in the worktree's own git directory; cleanup and reap treat such a
//! file as orq's only while it still matches, so an edit made afterwards counts as the agent's work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::outcome::git_raw;

/// Ignored paths orq itself writes (or links) into a run's worktree.
const DOTENV: &str = concat!(".", "env");
const CANDIDATES: [&str; 6] = [
    ".mcp.json",
    ".mcpctl.toml",
    ".mcpctl.lock",
    ".mcpctl.scaffold.json",
    ".claude/settings.local.json",
    DOTENV,
];
/// The project's TDD config, carried in from the main checkout.
const TDD_CONFIG: &str = ".tdd-config.json";
const MANIFEST: &str = "orq-written.json";

/// What `path` is now: a link's target, or a regular file's content hash. `None` for anything else.
fn fingerprint(worktree: &Path, name: &str) -> Option<String> {
    let path = worktree.join(name);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        return Some(format!(
            "link:{}",
            std::fs::read_link(&path).ok()?.display()
        ));
    }
    // orq only ever links the dotenv file; a regular file of that name is not orq's.
    if !meta.is_file() || name == DOTENV {
        return None;
    }
    let out = git_raw(worktree, &["hash-object", "--no-filters", "--", name]).ok()?;
    out.success.then(|| format!("blob:{}", out.stdout.trim()))
}

fn manifest_path(worktree: &Path) -> Option<PathBuf> {
    let out = git_raw(worktree, &["rev-parse", "--absolute-git-dir"]).ok()?;
    out.success
        .then(|| PathBuf::from(out.stdout.trim()).join(MANIFEST))
}

/// Records what orq wrote into `worktree`, so cleanup and reap can tell it from the agent's own work.
/// Returns a note on a problem.
pub fn record_orq_written(worktree: &Path) -> Vec<String> {
    let Some(path) = manifest_path(worktree) else {
        return vec!["what orq wrote into the worktree was not recorded: its git directory could not be found".to_string()];
    };
    // Every file `mcpctl init` lists as scaffolded is orq's too, except the TDD config: that is orq's
    // only as the copy orq carried in, which carrying recorded when it copied it. A config the
    // worktree already had stays the worktree's own, whatever it holds or lists it.
    let scaffolded = crate::mcp_setup::scaffolded_files(worktree)
        .into_iter()
        .filter(|name| name != TDD_CONFIG);
    let names = CANDIDATES
        .iter()
        .map(|name| name.to_string())
        .chain(scaffolded);
    let mut recorded: BTreeMap<String, String> = names
        .filter_map(|name| fingerprint(worktree, &name).map(|print| (name, print)))
        .collect();
    if let Some(print) = Recorded::load(worktree).carried_tdd_config(worktree) {
        recorded.insert(TDD_CONFIG.to_string(), print);
    }
    match write_manifest(&path, &recorded) {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!(
            "what orq wrote into the worktree was not recorded: {why}"
        )],
    }
}

fn write_manifest(path: &Path, recorded: &BTreeMap<String, String>) -> Result<(), String> {
    let text = serde_json::to_string(recorded).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{:?}", e.kind()))
}

/// Records `name` again after orq rewrote it, keeping every other file's entry as recorded. A file
/// that is gone loses its entry. Returns a note on a problem.
pub(crate) fn record_orq_written_file(worktree: &Path, name: &str) -> Vec<String> {
    let Some(path) = manifest_path(worktree) else {
        return vec![format!("orq's rewrite of {name} was not recorded: the worktree's git directory could not be found")];
    };
    let mut recorded: BTreeMap<String, String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    match fingerprint(worktree, name) {
        Some(print) => recorded.insert(name.to_string(), print),
        None => recorded.remove(name),
    };
    match write_manifest(&path, &recorded) {
        Ok(()) => Vec::new(),
        Err(why) => vec![format!("orq's rewrite of {name} was not recorded: {why}")],
    }
}

/// Whether `name` is one of the files orq writes into a worktree as content, so a copy that no
/// longer matches the record differs from what orq last wrote. The dotenv file is only ever a link,
/// and a regular file of that name was never orq's.
pub(crate) fn is_orq_file_name(name: &str) -> bool {
    name != DOTENV && CANDIDATES.contains(&name)
}

/// Whether `name` is something orq recorded writing and is still exactly that.
pub(crate) fn is_unchanged_orq_file(worktree: &Path, name: &str) -> bool {
    Recorded::load(worktree).unchanged(worktree, name) == Some(true)
}

/// What orq recorded writing into a worktree, read once.
#[derive(Default)]
pub(crate) struct Recorded(BTreeMap<String, String>);

impl Recorded {
    /// The record; empty when it is missing or malformed, so nothing is taken for orq's.
    pub(crate) fn load(worktree: &Path) -> Self {
        let text = manifest_path(worktree).and_then(|path| std::fs::read_to_string(path).ok());
        Self(
            text.and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default(),
        )
    }

    /// The TDD config's fingerprint while it is still the copy orq carried in and recorded.
    fn carried_tdd_config(&self, worktree: &Path) -> Option<String> {
        let print = fingerprint(worktree, TDD_CONFIG)?;
        (self.0.get(TDD_CONFIG) == Some(&print)).then_some(print)
    }

    /// For a file orq recorded writing, whether it is still exactly that; `None` for any other file.
    pub(crate) fn unchanged(&self, worktree: &Path, name: &str) -> Option<bool> {
        let print = self.0.get(name)?;
        Some(fingerprint(worktree, name).as_ref() == Some(print))
    }
}
