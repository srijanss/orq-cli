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
    if let Some(work) = uncommitted_work(&record.worktree)? {
        return keep(store, &record, work.reason(), true);
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
    /// Something is uncommitted: `path` is the first path git listed for it, and `hint` says, when it
    /// can, what would have let it go with the worktree. Both are printable.
    Present { path: String, hint: Option<String> },
}

impl UncommittedWork {
    /// Why the worktree stays, for a person to read.
    pub(crate) fn reason(&self) -> String {
        match self {
            UncommittedWork::Unreadable => "the worktree state could not be read".to_string(),
            UncommittedWork::Present { path, hint: None } => {
                format!("the worktree has uncommitted changes: {path}")
            }
            UncommittedWork::Present { path, hint: Some(hint) } => {
                format!("the worktree has uncommitted changes: {path} ({hint})")
            }
        }
    }
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
    let mut installs: HashMap<(String, &'static str), Result<(), String>> = HashMap::new();
    let mut own = |line: &str| {
        line.strip_prefix("!! ").is_some_and(|path| {
            // A folder git lists whole (a repository nested in it) is exempt only below a tagged cache
            // directory: anywhere else it may hold commits that exist nowhere else.
            in_cache_dir(worktree, path, &mut tagged)
                || (!path.ends_with('/')
                    && (in_installed_deps(worktree, path, &mut installs)
                        || crate::orq_written::is_unchanged_orq_file(worktree, path)
                        || is_tooling_state(path)
                        || is_tooling_folder(path)))
        })
    };
    let Some(entry) = status.stdout.split('\0').find(|entry| !entry.is_empty() && !own(entry)) else {
        return Ok(None);
    };
    // An entry is "XY path": the two status letters, a space, then the path.
    let path = entry.get(3..).unwrap_or(entry);
    let hint = if entry.starts_with("!! ") { ignored_hint(worktree, path, &installs)? } else { None };
    Ok(Some(UncommittedWork::Present { path: printable(path), hint: hint.as_deref().map(printable) }))
}

/// What would have let the ignored `path` go with the worktree: what its install folder lacks, or,
/// for a file in some other ignored folder, the cache tag. `None` for an ignored file in no ignored folder.
fn ignored_hint(
    worktree: &Path,
    path: &str,
    installs: &HashMap<(String, &'static str), Result<(), String>>,
) -> Result<Option<String>, StoreError> {
    let install = (!path.ends_with('/'))
        .then(|| install_folder_of(path))
        .flatten()
        .and_then(|(project, folder)| installs.get(&(project.to_string(), folder.name)));
    if let Some(Err(missing)) = install {
        return Ok(Some(missing.clone()));
    }
    Ok(in_ignored_folder(worktree, path)?
        .then(|| "an ignored folder holding a CACHEDIR.TAG is removed with the worktree".to_string()))
}

/// Whether one of the folders `path` lies in is itself ignored, so tagging it as a cache would let it go.
fn in_ignored_folder(worktree: &Path, path: &str) -> Result<bool, StoreError> {
    let folders: Vec<String> = Path::new(path.trim_end_matches('/'))
        .ancestors()
        .skip(1)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| format!("{}/", dir.to_string_lossy()))
        .collect();
    if folders.is_empty() {
        return Ok(false);
    }
    let mut args = vec!["check-ignore", "--"];
    args.extend(folders.iter().map(String::as_str));
    // It succeeds when at least one of the folders is ignored.
    Ok(git_raw(worktree, &args)?.success)
}

/// `name` with control characters (a newline, an escape sequence) written out as `\n`, `\u{1b}`, so a
/// file name cannot forge lines or move the cursor where the reason is shown.
fn printable(name: &str) -> String {
    name.chars()
        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
        .collect()
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

/// A folder a package manager installs dependencies into, rebuildable from its project folder's
/// manifest and lockfile.
struct InstallFolder {
    name: &'static str,
    /// The manifests, any one of which the project folder must have committed.
    manifests: &'static [&'static str],
    /// The lockfiles, any one of which the project folder must have committed.
    lockfiles: &'static [&'static str],
}

const INSTALL_FOLDERS: &[InstallFolder] = &[
    InstallFolder {
        name: "node_modules",
        manifests: &["package.json"],
        lockfiles: &["package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "pnpm-lock.yaml", "bun.lock", "bun.lockb"],
    },
    InstallFolder {
        name: ".venv",
        manifests: &["pyproject.toml", "Pipfile"],
        lockfiles: &["uv.lock", "poetry.lock", "pdm.lock", "Pipfile.lock"],
    },
];

/// The outermost install folder the file `path` lies in, with its project folder (the folder that
/// holds it, `""` at the top of the worktree).
fn install_folder_of(path: &str) -> Option<(&str, &'static InstallFolder)> {
    let mut start = 0;
    // The last component is the file itself, never a folder.
    let (folders, _file) = path.rsplit_once('/')?;
    for part in folders.split('/') {
        if let Some(folder) = INSTALL_FOLDERS.iter().find(|f| f.name == part) {
            return Some((path[..start].trim_end_matches('/'), folder));
        }
        start += part.len() + 1;
    }
    None
}

/// Whether the file `path` (relative to `worktree`) lies in an install folder whose project folder has
/// a manifest and a lockfile committed at `HEAD`: the folder can be installed again from them. Each
/// project folder is asked once: an install folder lists thousands of files.
fn in_installed_deps(
    worktree: &Path,
    path: &str,
    installs: &mut HashMap<(String, &'static str), Result<(), String>>,
) -> bool {
    let Some((project, folder)) = install_folder_of(path) else {
        return false;
    };
    installs
        .entry((project.to_string(), folder.name))
        .or_insert_with(|| install_missing(worktree, project, folder))
        .is_ok()
}

/// `Ok` when `project` has one of `folder`'s manifests and one of its lockfiles committed at `HEAD`;
/// otherwise what is missing, as a hint for the kept reason.
fn install_missing(worktree: &Path, project: &str, folder: &InstallFolder) -> Result<(), String> {
    let prefix = if project.is_empty() { String::new() } else { format!("{project}/") };
    // The project folder's own entries only, not its subfolders'; the whole top level for the worktree's.
    let mut args = vec!["ls-tree", "-z", "--full-tree", "HEAD"];
    if !project.is_empty() {
        args.extend(["--", prefix.as_str()]);
    }
    let committed: Vec<String> = match git_raw(worktree, &args) {
        // Each entry is "mode type sha\tpath"; only a plain file counts. A link (mode 120000) is a
        // blob too, but what it points at may be uncommitted, even inside the install folder itself.
        Ok(out) if out.success => out
            .stdout
            .split('\0')
            .filter_map(|entry| entry.split_once('\t'))
            .filter(|(meta, _)| matches!(meta.split(' ').next(), Some("100644" | "100755")))
            .filter_map(|(_, name)| name.strip_prefix(&prefix).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let has = |names: &[&str]| names.iter().any(|n| committed.iter().any(|c| c == n));
    if has(folder.manifests) && has(folder.lockfiles) {
        return Ok(());
    }
    // A lockfile on disk but not committed is the one to name: committing it is all that is missing.
    let on_disk = folder.lockfiles.iter().find(|lock| worktree.join(format!("{prefix}{lock}")).is_file());
    let what = format!(
        "{prefix}{} is removed with the worktree only when {prefix}{} and a lockfile such as {prefix}{} are committed",
        folder.name,
        folder.manifests[0],
        on_disk.unwrap_or(&folder.lockfiles[0]),
    );
    Err(match on_disk {
        _ if !has(folder.manifests) => format!("{what}; {prefix}{} is not committed", folder.manifests[0]),
        Some(lock) => format!("{what}; {prefix}{lock} is not committed"),
        None => format!("{what}; no lockfile is committed"),
    })
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
