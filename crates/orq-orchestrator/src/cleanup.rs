//! Removing a run's worktree once the run has finished well.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ci_wait::CiOutcome;
use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "cleanup";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// The worktree directory and its registration are gone.
    Removed,
    /// The worktree stays where it is, for this reason.
    Kept { reason: String },
}

/// Remove the worktree of `run_id` if the run reached `review_ready` with CI green (or
/// CI disabled). A blocked or failed run, a red or unknown CI result, and a worktree
/// with uncommitted work are all kept. Only the worktree goes: the branch and the
/// pushed review request are never touched.
pub fn cleanup_worktree(
    store: &RunStore,
    run_id: &str,
    review_ready: bool,
    ci: &CiOutcome,
) -> Result<CleanupOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !review_ready {
        return keep(
            store,
            &record,
            "the run did not reach review_ready".to_string(),
            false,
        );
    }
    if !matches!(ci, CiOutcome::Passed | CiOutcome::Skipped) {
        return keep(store, &record, "CI did not pass".to_string(), false);
    }
    // Anything uncommitted is work that removal would lose: untracked files, and ignored
    // ones too, since `git worktree remove` deletes ignored files without --force.
    match uncommitted_work(&record.worktree)? {
        Some(UncommittedWork::Unreadable) => {
            return keep(
                store,
                &record,
                "the worktree state could not be read".to_string(),
                true,
            )
        }
        Some(UncommittedWork::Present(path)) => {
            return keep(
                store,
                &record,
                format!("the worktree has uncommitted changes: {path}"),
                true,
            )
        }
        None => {}
    }
    let common = git_raw(
        &record.worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !common.success {
        return keep(
            store,
            &record,
            "the repository of the worktree could not be found".to_string(),
            true,
        );
    }
    let worktree = record.worktree.to_string_lossy().into_owned();
    // Run from the repository's own git dir: the worktree being removed cannot be the working directory.
    // No --force: git itself refuses a locked or modified worktree.
    let removed = git_raw(
        Path::new(common.stdout.trim()),
        &["worktree", "remove", "--", &worktree],
    )?;
    if !removed.success {
        return keep(
            store,
            &record,
            format!(
                "git would not remove the worktree: {}",
                removed.stderr.trim()
            ),
            true,
        );
    }
    record_event(
        store,
        &record,
        "worktree.removed",
        serde_json::json!({ "worktree": worktree }),
    )?;
    Ok(CleanupOutcome::Removed)
}

pub(crate) enum UncommittedWork {
    Unreadable,
    /// Something is uncommitted; this is the first path git listed for it.
    Present(String),
}

/// Whether `worktree` holds anything uncommitted besides the ignored files orq wrote itself.
pub(crate) fn uncommitted_work(worktree: &Path) -> Result<Option<UncommittedWork>, StoreError> {
    let status = git_raw(
        worktree,
        &[
            "status",
            "--porcelain",
            // NUL-delimited: paths come out as they are, not quoted and escaped.
            "-z",
            "--untracked-files=all",
            "--ignored",
        ],
    )?;
    // git_raw reads output lossily: a path that is not UTF-8 comes back as another name, which could
    // be taken for a cache directory of its own. Such a worktree is not judged, but kept.
    if !status.success || status.stdout.contains(char::REPLACEMENT_CHARACTER) {
        return Ok(Some(UncommittedWork::Unreadable));
    }
    // Untracked files are listed one by one, so an ignored directory is never exempt as a whole: each
    // file in it must be one orq recorded writing and still unchanged, or sit in a build cache.
    // Whether each directory holds a cache tag is looked up once: a build cache lists thousands of files.
    let mut tagged: HashMap<PathBuf, bool> = HashMap::new();
    let mut own = |line: &str| {
        line.strip_prefix("!! ").is_some_and(|path| {
            !path.ends_with('/')
                && (in_cache_dir(worktree, path, &mut tagged)
                    || crate::orq_written::is_unchanged_orq_file(worktree, path)
                    || is_tooling_state(path)
                    || is_tooling_folder(path))
        })
    };
    Ok(status
        .stdout
        .split('\0')
        .find(|entry| !entry.is_empty() && !own(entry))
        // An entry is "XY path": the two status letters, a space, then the path.
        .map(|entry| UncommittedWork::Present(entry.get(3..).unwrap_or(entry).to_string())))
}

/// Whether `path` is state the TDD and MCP tooling keeps in the worktree while a run works (`.tdd-*`,
/// `.project-mcp/`). The tooling rewrites it during the run, so it cannot be fingerprinted at setup; it
/// is known by name, and losing it loses no work. The files orq itself writes at setup (`.mcp.json`,
/// `.mcpctl*`) are not here: they are exempt only while unchanged since orq recorded them.
fn is_tooling_state(path: &str) -> bool {
    let top = path.split('/').next().unwrap_or(path);
    let orq_written = top == ".mcp.json" || top.starts_with(".mcpctl");
    !orq_written && orq_core::ownership::classify(top) == orq_core::ownership::Ownership::Mcp
}

/// Whether `path` lies in a folder the agent tooling installs into a session's worktree (skills, hooks,
/// agent and command definitions). `.claude/worktrees` is not one: it holds other runs' worktrees.
fn is_tooling_folder(path: &str) -> bool {
    let mut parts = path.split('/');
    match parts.next() {
        Some(".agents" | ".codex" | ".pi") => true,
        Some(".claude") => matches!(parts.next(), Some("agents" | "commands" | "settings.json")),
        _ => false,
    }
}

/// The first line of the `CACHEDIR.TAG` that cargo and other tools put in a directory of rebuildable
/// output (https://bford.info/cachedir/).
const CACHEDIR_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `path` (relative to `worktree`) lies in a directory tagged as a cache: whatever is
/// there can be rebuilt, so losing it with the worktree loses no work.
fn in_cache_dir(worktree: &Path, path: &str, tagged: &mut HashMap<PathBuf, bool>) -> bool {
    Path::new(path).ancestors().skip(1).any(|dir| {
        *tagged
            .entry(dir.to_path_buf())
            .or_insert_with(|| is_cache_tag(&worktree.join(dir).join("CACHEDIR.TAG")))
    })
}

/// Whether `tag` is a plain file that begins with the cache signature. It is opened without following
/// a link and without waiting on a pipe, then judged by the handle itself: the path may have been
/// swapped for something else since it was listed.
fn is_cache_tag(tag: &Path) -> bool {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(tag)
    else {
        return false;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = Vec::new();
    file.take(CACHEDIR_SIGNATURE.len() as u64).read_to_end(&mut head).is_ok() && head == CACHEDIR_SIGNATURE.as_bytes()
}

/// `run_succeeded` says whether the run itself finished well, apart from why the worktree stays.
fn keep(
    store: &RunStore,
    record: &RunRecord,
    reason: String,
    run_succeeded: bool,
) -> Result<CleanupOutcome, StoreError> {
    let worktree = record.worktree.to_string_lossy().into_owned();
    record_event(
        store,
        record,
        "worktree.kept",
        serde_json::json!({ "worktree": worktree, "reason": reason, "run_succeeded": run_succeeded }),
    )?;
    Ok(CleanupOutcome::Kept { reason })
}

fn record_event(
    store: &RunStore,
    record: &RunRecord,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(&record.run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: kind.to_string(),
        stage: Some(STAGE.to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload,
    })
}
