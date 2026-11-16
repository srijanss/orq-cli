//! The worktrees orq made and then kept: listed with what is in them, so a person can see what
//! deleting one would lose.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cleanup::{printable, record_event, scan_work};
use crate::outcome::git_raw;
use crate::reap::within_worktrees_dir;
use crate::run_status::{summarize_run, RunPhase};
use crate::store::{Event, RunRecord, RunStore, StoreError};

/// How many changed files an entry names; the rest are counted.
const SHOWN_FILES: usize = 20;
/// How long the size of one worktree may take to measure before it is left unknown.
const SIZE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A tracked file with changes not committed.
    Tracked,
    Untracked,
    /// An ignored file orq, the tooling or a build did not leave there.
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Printable: control characters are written out.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptWorktree {
    pub branch: String,
    pub path: PathBuf,
    /// The commit the worktree is on.
    pub head: String,
    /// The newest run recorded on the worktree, when its record exists.
    pub run_id: Option<String>,
    /// How that run ended, as [`RunPhase::as_str`] says it.
    pub ending: Option<String>,
    /// Why cleanup kept the worktree, from its last `worktree.kept` event.
    pub kept_reason: Option<String>,
    /// The branch the commits are counted against.
    pub target: String,
    /// Commits on the worktree that are not on the target; `None` when git could not say.
    pub commits: Option<u64>,
    /// The first changed files.
    pub files: Vec<ChangedFile>,
    /// How many more changed files there are than `files` names.
    pub more_files: usize,
    /// The worktree's status could not be read, so what it holds is unknown.
    pub files_unreadable: bool,
    /// Every changed file, shown or counted, is an ignored one.
    pub only_ignored: bool,
    /// Bytes on disk; `None` when they could not be measured in time.
    pub size: Option<u64>,
    /// A hash of the head, the commit count and the changed files as listed: a delete names it, so
    /// only what was shown goes.
    pub version: String,
}

impl KeptWorktree {
    /// Every commit on it is on the target.
    pub fn merged(&self) -> bool {
        self.commits == Some(0)
    }

    /// Merged, and nothing in it but ignored files: deleting it loses no commit and no tracked work.
    pub fn safe(&self) -> bool {
        self.merged() && !self.files_unreadable && self.only_ignored
    }
}

/// One `git worktree list` entry.
struct Listed {
    path: PathBuf,
    head: String,
    branch: Option<String>,
}

fn listed_worktrees(repo: &Path) -> Result<Vec<Listed>, StoreError> {
    let out = git_raw(repo, &["worktree", "list", "--porcelain", "-z"])?;
    if !out.success {
        return Err(StoreError::Corrupt(format!("git could not list the worktrees: {}", out.stderr.trim())));
    }
    let mut listed = Vec::new();
    let mut current: Option<Listed> = None;
    for field in out.stdout.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            listed.extend(current.take());
            current = Some(Listed { path: PathBuf::from(path), head: String::new(), branch: None });
        } else if let Some(entry) = current.as_mut() {
            if let Some(head) = field.strip_prefix("HEAD ") {
                entry.head = head.to_string();
            } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
                entry.branch = Some(branch.to_string());
            }
        }
    }
    listed.extend(current);
    Ok(listed)
}

fn branch_prefix(repo: &Path) -> String {
    crate::run_settings::load_run_settings(repo, &|name| std::env::var(name).ok())
        .ok()
        .and_then(|settings| settings.branch_prefix)
        .unwrap_or_else(|| orq_core::naming::DEFAULT_BRANCH_PREFIX.to_string())
}

fn is_active(phase: RunPhase) -> bool {
    matches!(phase, RunPhase::Running | RunPhase::PauseRequested | RunPhase::Paused)
}

/// A run recorded on a worktree, with its events.
struct Recorded {
    record: RunRecord,
    events: Vec<Event>,
}

fn records(store: &RunStore) -> Result<Vec<Recorded>, StoreError> {
    let mut found = Vec::new();
    for run_id in store.run_ids()? {
        let Ok(record) = store.load(&run_id) else { continue };
        // Events that cannot be read say nothing about the run: it counts as still going.
        let events = store.events(&run_id).ok();
        found.push((record, events));
    }
    Ok(found
        .into_iter()
        .map(|(record, events)| Recorded {
            events: events.unwrap_or_else(|| vec![running_marker(&record)]),
            record,
        })
        .collect())
}

/// An event that leaves a run running, for a run whose events cannot be read.
fn running_marker(record: &RunRecord) -> Event {
    Event {
        schema_version: 1,
        event_id: String::new(),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "stage.started".to_string(),
        stage: None,
        attempt: 1,
        occurred_at: String::new(),
        payload: serde_json::Value::Null,
    }
}

/// Whether some active run holds the worktree: one recorded on it, or one holding the feature lock of
/// a run recorded on it.
fn held(store: &RunStore, all: &[Recorded], mine: &[&Recorded]) -> Result<bool, StoreError> {
    for run in mine {
        if is_active(summarize_run(&run.events).phase) {
            return Ok(true);
        }
        if let Some(owner) = store.active_run(&run.record.project_id, &run.record.feature_id)? {
            let owner_active = match all.iter().find(|r| r.record.run_id == owner) {
                Some(owner) => is_active(summarize_run(&owner.events).phase),
                // A lock whose run has no record: nothing says it is over.
                None => true,
            };
            if owner_active {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn same_place(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Every worktree of `repo` that orq made and no active run holds: inside the repository's worktrees
/// folder, on a branch with the run prefix or a recorded run's branch.
pub fn list_kept_worktrees(store: &RunStore, repo: &Path) -> Result<Vec<KeptWorktree>, StoreError> {
    let all = records(store)?;
    let prefix = branch_prefix(repo);
    let mut kept = Vec::new();
    for listed in listed_worktrees(repo)?.into_iter().skip(1) {
        let Some(branch) = listed.branch.clone() else { continue };
        if same_place(&listed.path, repo) || !within_worktrees_dir(repo, &listed.path) {
            continue;
        }
        let mine = recorded_on(&all, &branch, &listed.path);
        if !branch.starts_with(&prefix) && mine.is_empty() {
            continue;
        }
        if held(store, &all, &mine)? {
            continue;
        }
        kept.push(describe(repo, &listed, branch, mine.last().copied(), true)?);
    }
    kept.sort_by(|a, b| a.branch.cmp(&b.branch));
    Ok(kept)
}

fn recorded_on<'a>(all: &'a [Recorded], branch: &str, path: &Path) -> Vec<&'a Recorded> {
    all.iter()
        .filter(|run| run.record.branch == branch || same_place(&run.record.worktree, path))
        .collect()
}

/// Why a kept worktree was not deleted. Nothing was deleted in any of these cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteRefusal {
    /// No worktree orq made is on that branch.
    NotFound,
    /// The worktree's path is not one orq may delete: why.
    Unsafe(String),
    /// An active run holds the worktree or its feature.
    Active,
    /// What the worktree holds is not what the caller saw.
    Changed,
    /// It has commits not on the target or files that are not ignored, and force was not given.
    NeedsForce,
}

impl std::fmt::Display for DeleteRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteRefusal::NotFound => write!(f, "no kept worktree is on that branch; nothing was deleted"),
            DeleteRefusal::Unsafe(why) => write!(f, "{why}; nothing was deleted"),
            DeleteRefusal::Active => write!(f, "a run still uses the worktree; nothing was deleted"),
            DeleteRefusal::Changed => write!(f, "the worktree changed since you opened it; nothing was deleted"),
            DeleteRefusal::NeedsForce => {
                write!(f, "the worktree has commits or files that would be lost; confirm to delete them")
            }
        }
    }
}

/// What a delete removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedWorktree {
    pub branch: String,
    pub path: PathBuf,
    /// The commit the branch pointed at.
    pub sha: String,
    pub run_id: Option<String>,
}

/// Delete the kept worktree on `branch` and the branch itself, if it is still what `version` says
/// and, unless `force`, holds nothing but ignored files and commits on its target.
pub fn delete_kept_worktree(
    store: &RunStore,
    repo: &Path,
    branch: &str,
    version: &str,
    force: bool,
) -> Result<Result<DeletedWorktree, DeleteRefusal>, StoreError> {
    let all = records(store)?;
    let prefix = branch_prefix(repo);
    let Some(listed) = listed_worktrees(repo)?
        .into_iter()
        .skip(1)
        .find(|listed| listed.branch.as_deref() == Some(branch))
    else {
        return Ok(Err(DeleteRefusal::NotFound));
    };
    let mine = recorded_on(&all, branch, &listed.path);
    if !branch.starts_with(&prefix) && mine.is_empty() {
        return Ok(Err(DeleteRefusal::NotFound));
    }
    if let Some(why) = unsafe_path(repo, &listed.path) {
        return Ok(Err(DeleteRefusal::Unsafe(why)));
    }
    if held(store, &all, &mine)? {
        return Ok(Err(DeleteRefusal::Active));
    }
    let run = mine.last().copied();
    let entry = describe(repo, &listed, branch.to_string(), run, false)?;
    if entry.version != version {
        return Ok(Err(DeleteRefusal::Changed));
    }
    if !entry.safe() && !force {
        return Ok(Err(DeleteRefusal::NeedsForce));
    }
    let path = listed.path.to_string_lossy().to_string();
    let removed = git_raw(repo, &["worktree", "remove", "--force", "--", &path])?;
    if !removed.success {
        return Err(StoreError::Corrupt(format!("git could not remove the worktree: {}", removed.stderr.trim())));
    }
    let dropped = git_raw(repo, &["update-ref", "-d", &format!("refs/heads/{branch}"), &listed.head])?;
    if !dropped.success {
        return Err(StoreError::Corrupt(format!(
            "the worktree was removed but git could not delete its branch: {}",
            dropped.stderr.trim()
        )));
    }
    // A failed prune leaves only stale bookkeeping behind; the worktree and branch are gone.
    let _ = git_raw(repo, &["worktree", "prune"])?;
    if let Some(run) = run {
        record_event(
            store,
            &run.record,
            "worktree.deleted",
            serde_json::json!({ "worktree": listed.path, "branch": branch, "sha": listed.head }),
        )?;
    }
    Ok(Ok(DeletedWorktree {
        branch: branch.to_string(),
        path: listed.path,
        sha: listed.head,
        run_id: run.map(|run| run.record.run_id.clone()),
    }))
}

/// Why `path` is not a worktree orq may delete, if it is not: outside the worktrees folder, the
/// main worktree, written with `..`, or reached through a symlink.
fn unsafe_path(repo: &Path, path: &Path) -> Option<String> {
    if path.components().any(|part| part == std::path::Component::ParentDir) {
        return Some("the worktree's path goes up a folder".to_string());
    }
    let mut step = repo.to_path_buf();
    for part in [".claude", "worktrees"] {
        step.push(part);
        if step.symlink_metadata().map_or(true, |meta| meta.file_type().is_symlink()) {
            return Some("the worktrees folder is a symlink or missing".to_string());
        }
    }
    let Ok(below) = path.strip_prefix(&step) else {
        return Some("the worktree is not inside the worktrees folder".to_string());
    };
    for part in below.components() {
        step.push(part);
        if step.symlink_metadata().map_or(true, |meta| meta.file_type().is_symlink()) {
            return Some("the worktree's path is a symlink or missing".to_string());
        }
    }
    if same_place(path, repo) || !within_worktrees_dir(repo, path) {
        return Some("the worktree is not inside the worktrees folder".to_string());
    }
    None
}

fn describe(
    repo: &Path,
    listed: &Listed,
    branch: String,
    run: Option<&Recorded>,
    measure: bool,
) -> Result<KeptWorktree, StoreError> {
    let target = match run {
        Some(run) => run.record.target_branch.clone(),
        None => current_branch(repo)?.unwrap_or_else(|| "main".to_string()),
    };
    let counted = git_raw(&listed.path, &["rev-list", "--count", &format!("{target}..HEAD"), "--"])?;
    let commits = counted.success.then(|| counted.stdout.trim().parse::<u64>().ok()).flatten();
    let (files, more_files, only_ignored, files_unreadable) = match scan_work(&listed.path, None)? {
        Ok(Some(scan)) => {
            let all: Vec<ChangedFile> = scan.entries.iter().map(|entry| changed_file(entry)).collect();
            let only_ignored = all.iter().all(|file| file.kind == ChangeKind::Ignored);
            let more = all.len().saturating_sub(SHOWN_FILES);
            (all.into_iter().take(SHOWN_FILES).collect(), more, only_ignored, false)
        }
        _ => (Vec::new(), 0, false, true),
    };
    let version = version_of(&listed.head, commits, &files, more_files, files_unreadable);
    Ok(KeptWorktree {
        branch,
        path: listed.path.clone(),
        head: listed.head.clone(),
        run_id: run.map(|run| run.record.run_id.clone()),
        ending: run.map(|run| summarize_run(&run.events).phase.as_str().to_string()),
        kept_reason: run.and_then(|run| kept_reason(&run.events)),
        target,
        commits,
        files,
        more_files,
        files_unreadable,
        only_ignored,
        size: measure.then(|| size_within(&listed.path, SIZE_BUDGET)).flatten(),
        version,
    })
}

fn current_branch(repo: &Path) -> Result<Option<String>, StoreError> {
    let out = git_raw(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    Ok(out.success.then(|| out.stdout.trim().to_string()).filter(|b| !b.is_empty()))
}

fn kept_reason(events: &[Event]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == "worktree.kept")
        .and_then(|event| event.payload.get("reason")?.as_str().map(printable))
}

/// A status entry, "XY path", as a changed file.
fn changed_file(entry: &str) -> ChangedFile {
    let kind = match entry.get(..2) {
        Some("!!") => ChangeKind::Ignored,
        Some("??") => ChangeKind::Untracked,
        _ => ChangeKind::Tracked,
    };
    ChangedFile { path: printable(entry.get(3..).unwrap_or(entry)), kind }
}

fn version_of(head: &str, commits: Option<u64>, files: &[ChangedFile], more: usize, unreadable: bool) -> String {
    let mut text = format!("{head}\n{commits:?}\n{more}\n{unreadable}\n");
    for file in files {
        text.push_str(&format!("{:?} {}\n", file.kind, file.path));
    }
    format!("{:016x}", fnv1a(text.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The bytes of the files under `root`, symlinks not followed; `None` past `budget`.
fn size_within(root: &Path, budget: Duration) -> Option<u64> {
    let deadline = Instant::now() + budget;
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            if Instant::now() > deadline {
                return None;
            }
            let entry = entry.ok()?;
            let meta = entry.path().symlink_metadata().ok()?;
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Some(total)
}
