//! A merge-local run delivers its work without a remote: the base branch of the run's own
//! repository is fast-forwarded to the run's branch. Never a merge commit, never a push.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::cleanup::printable;
use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::{git_bytes, git_raw};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "merge_local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The base branch now points at the run's head.
    Merged { head_sha: String },
    /// The base branch was left where it was, and why.
    Refused { reason: String },
    /// The base branch moved past the commit the run was rebased onto, to `base_sha`, and was
    /// left there: rebasing the run onto `base_sha` makes the fast-forward possible again.
    BaseMoved { base_sha: String },
}

/// Fast-forward `run_id`'s target branch to the run's branch, in the checkout that has it.
pub fn merge_into_base(store: &RunStore, run_id: &str) -> Result<MergeOutcome, StoreError> {
    let record = store.load(run_id)?;
    let (head_sha, base_sha) = tips(&record)?;
    let target = format!("refs/heads/{}", record.target_branch);
    let fast_forward = git_raw(&record.worktree, &["merge-base", "--is-ancestor", &base_sha, &head_sha])?;
    if !fast_forward.success {
        return Ok(MergeOutcome::BaseMoved { base_sha });
    }
    let mut kept_dirty = 0;
    match checkout_of(&record)? {
        Some(checkout) => {
            let changed = match changed_paths(&record.worktree, &base_sha, &head_sha)? {
                Ok(changed) => changed,
                Err(reason) => return Ok(MergeOutcome::Refused { reason }),
            };
            let dirty = match dirty_paths(&checkout)? {
                Ok(dirty) => dirty,
                Err(reason) => return Ok(MergeOutcome::Refused { reason }),
            };
            // Escaped, so a file name cannot forge lines or move the cursor where it is shown.
            let overlapping: Vec<String> = dirty.intersection(&changed).map(|path| printable(&path.to_string_lossy())).collect();
            if !overlapping.is_empty() {
                let files = named(&overlapping);
                record_event(store, &record, "merge.refused", serde_json::json!({ "reason": "dirty_checkout", "files": overlapping }))?;
                return Ok(MergeOutcome::Refused {
                    reason: format!(
                        "{} is checked out at {} with uncommitted changes to files this run changes: {files}; commit or stash them, then resume this run",
                        record.target_branch,
                        checkout.display()
                    ),
                });
            }
            kept_dirty = dirty.len();
            // Read only now the merge goes ahead: a refusal needs no file's contents.
            let before = match contents(&checkout, dirty)? {
                Ok(before) => before,
                Err(reason) => return Ok(MergeOutcome::Refused { reason }),
            };
            // A local file in the way, ignored or untracked, makes git stop rather than overwrite it.
            let merged = git_raw(&checkout, &["merge", "--ff-only", "--no-overwrite-ignore", "--quiet", &head_sha])?;
            if !merged.success {
                if let Some(base_sha) = moved_on_from(&record, &base_sha)? {
                    // A fast-forward updates the checkout before the branch: when the branch had
                    // moved by then, the checkout may hold this run's files over the new commit,
                    // which only a person can tell from their own edits.
                    // The person's own edits are not what it left: only a path dirty now and
                    // not before, or with other content than before, is.
                    let now = match dirty_paths(&checkout)? {
                        Ok(now) => contents(&checkout, now)?,
                        Err(reason) => Err(reason),
                    };
                    let now = match now {
                        Ok(now) => now,
                        Err(reason) => return Ok(MergeOutcome::Refused { reason }),
                    };
                    if now.iter().any(|(path, content)| before.get(path) != Some(content)) {
                        return Ok(MergeOutcome::Refused {
                            reason: format!(
                                "{} moved while {} was being fast-forwarded, and the checkout was left with changes; check them, then resume this run",
                                record.target_branch,
                                checkout.display()
                            ),
                        });
                    }
                    return Ok(MergeOutcome::BaseMoved { base_sha });
                }
                return Ok(MergeOutcome::Refused {
                    reason: format!("{} at {} could not be fast-forwarded: {}", record.target_branch, checkout.display(), merged.stderr.trim()),
                });
            }
        }
        // With no checkout to update, only the ref moves, and only from the commit checked above,
        // so a base branch that moved meanwhile is never overwritten.
        None => {
            let reason = format!("orq: merge-local {}", record.run_id);
            let moved = git_raw(&record.worktree, &["update-ref", "-m", &reason, &target, &head_sha, &base_sha])?;
            if !moved.success {
                if let Some(base_sha) = moved_on_from(&record, &base_sha)? {
                    return Ok(MergeOutcome::BaseMoved { base_sha });
                }
                return Ok(MergeOutcome::Refused { reason: format!("{} could not be moved to this run's branch: {}", record.target_branch, moved.stderr.trim()) });
            }
        }
    }
    let mut completed = serde_json::json!({ "head_sha": head_sha, "target_branch": record.target_branch });
    if kept_dirty > 0 {
        completed["kept_dirty"] = kept_dirty.into();
    }
    record_event(store, &record, "merge.completed", completed)?;
    Ok(MergeOutcome::Merged { head_sha })
}

/// How a merge that set the checkout's overlapping changes aside ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetAsideOutcome {
    /// The base branch now points at the run's head; `note` says what the person must do about
    /// changes that could not be put back cleanly.
    Merged { head_sha: String, note: Option<String> },
    /// The base branch was left where it was, with the checkout's changes put back, and why.
    Refused { reason: String },
    /// As [`MergeOutcome::BaseMoved`], with the checkout's changes put back.
    BaseMoved { base_sha: String },
}

/// Fast-forward `run_id`'s target branch as [`merge_into_base`] does, first stashing the checkout's
/// uncommitted changes to the files the run changes and applying them again afterwards.
pub fn merge_setting_aside(store: &RunStore, run_id: &str) -> Result<SetAsideOutcome, StoreError> {
    let record = store.load(run_id)?;
    if !waiting_on_dirty_checkout(&store.events(run_id)?) {
        return Ok(SetAsideOutcome::Refused { reason: "this run is not waiting on a dirty checkout".to_string() });
    }
    let Some(checkout) = checkout_of(&record)? else { return merged_as_is(store, run_id) };
    let (head_sha, base_sha) = tips(&record)?;
    let changed = match changed_paths(&record.worktree, &base_sha, &head_sha)? {
        Ok(changed) => changed,
        Err(reason) => return Ok(SetAsideOutcome::Refused { reason }),
    };
    let dirty = match dirty_paths(&checkout)? {
        Ok(dirty) => dirty,
        Err(reason) => return Ok(SetAsideOutcome::Refused { reason }),
    };
    let overlapping: Vec<&PathBuf> = dirty.intersection(&changed).collect();
    if overlapping.is_empty() {
        return merged_as_is(store, run_id);
    }
    let message = format!("orq set-aside {} {}", record.run_id, now_rfc3339());
    let mut args = ["--literal-pathspecs", "stash", "push", "--quiet", "-m", &message, "--"].map(OsStr::new).to_vec();
    args.extend(overlapping.iter().map(|path| path.as_os_str()));
    let pushed = git_bytes(&checkout, &args)?;
    if !pushed.status.success() {
        return Ok(SetAsideOutcome::Refused {
            reason: format!("cannot stash the uncommitted changes at {}: {}", checkout.display(), String::from_utf8_lossy(&pushed.stderr).trim()),
        });
    }
    let Some((_, stash)) = stash_entry(&stash_listing(&checkout)?, &message) else { return merged_as_is(store, run_id) };
    let files: Vec<String> = overlapping.iter().map(|path| printable(&path.to_string_lossy())).collect();
    match merge_into_base(store, run_id)? {
        MergeOutcome::Merged { head_sha } => {
            // Staged and unstaged changes go back where they were; when the run's commits leave no
            // room for the staged ones as they were, the changes go back unstaged, with the entry kept.
            let mut applied = git_raw(&checkout, &["stash", "apply", "--index", "--quiet", &stash])?;
            let unstaged = !applied.success && unmerged_paths(&checkout)?.is_empty();
            if unstaged {
                applied = git_raw(&checkout, &["stash", "apply", "--quiet", &stash])?;
            }
            if !applied.success {
                // Nothing is dropped after a failed apply: the entry is the person's only copy.
                let conflicted = unmerged_paths(&checkout)?;
                let label = format!("orq set-aside {}", record.run_id);
                let note = if conflicted.is_empty() {
                    format!("your changes could not be put back and are in stash {stash} ({label}): {}", applied.stderr.trim())
                } else {
                    let files = named(&conflicted);
                    format!("your changes to {files} conflicted with this run and are in stash {stash} ({label}); the checkout has conflict markers in {files}")
                };
                let payload = serde_json::json!({ "stash": stash, "restored": false, "files": files, "conflicted": conflicted, "note": note });
                record_event(store, &record, "merge.set_aside", payload)?;
                return Ok(SetAsideOutcome::Merged { head_sha, note: Some(note) });
            }
            if unstaged {
                // The entry is the only copy left of what was staged.
                let note = format!(
                    "your changes to {} were put back but no longer staged; as they were staged they are in stash {stash} (orq set-aside {})",
                    named(&files),
                    record.run_id
                );
                let payload = serde_json::json!({ "stash": stash, "restored": true, "staged": false, "files": files, "note": note });
                record_event(store, &record, "merge.set_aside", payload)?;
                return Ok(SetAsideOutcome::Merged { head_sha, note: Some(note) });
            }
            drop_stash(&checkout, &message, &stash)?;
            record_event(store, &record, "merge.set_aside", serde_json::json!({ "stash": stash, "restored": true, "files": files }))?;
            Ok(SetAsideOutcome::Merged { head_sha, note: None })
        }
        MergeOutcome::Refused { reason } => {
            let reason = match put_back(&checkout, &message, &stash, &record.run_id)? {
                None => reason,
                Some(kept) => format!("{reason}; {kept}"),
            };
            Ok(SetAsideOutcome::Refused { reason })
        }
        MergeOutcome::BaseMoved { base_sha } => match put_back(&checkout, &message, &stash, &record.run_id)? {
            None => Ok(SetAsideOutcome::BaseMoved { base_sha }),
            // Rebasing on would set aside again over changes that are not back yet.
            Some(kept) => Ok(SetAsideOutcome::Refused { reason: format!("{} moved while this run was being merged; {kept}", record.target_branch) }),
        },
    }
}

/// Apply the entry back, staged state and all, into the checkout the merge left as it was, and
/// drop it; `None` when that worked, or what to tell the person about the entry kept.
fn put_back(checkout: &Path, message: &str, stash: &str, run_id: &str) -> Result<Option<String>, StoreError> {
    let applied = git_raw(checkout, &["stash", "apply", "--index", "--quiet", stash])?;
    if !applied.success {
        return Ok(Some(format!("your changes could not be put back and are in stash {stash} (orq set-aside {run_id}): {}", applied.stderr.trim())));
    }
    drop_stash(checkout, message, stash)?;
    Ok(None)
}

/// Whether a run's last merge attempt was refused over uncommitted changes to files it changes.
pub fn waiting_on_dirty_checkout(events: &[Event]) -> bool {
    events
        .iter()
        .rev()
        .find(|event| event.kind.starts_with("merge."))
        .is_some_and(|event| event.kind == "merge.refused" && event.payload["reason"] == "dirty_checkout")
}

/// [`merge_into_base`], with nothing set aside.
fn merged_as_is(store: &RunStore, run_id: &str) -> Result<SetAsideOutcome, StoreError> {
    Ok(match merge_into_base(store, run_id)? {
        MergeOutcome::Merged { head_sha } => SetAsideOutcome::Merged { head_sha, note: None },
        MergeOutcome::Refused { reason } => SetAsideOutcome::Refused { reason },
        MergeOutcome::BaseMoved { base_sha } => SetAsideOutcome::BaseMoved { base_sha },
    })
}

/// The run's head and the base branch's commit.
fn tips(record: &RunRecord) -> Result<(String, String), StoreError> {
    let head = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.branch)])?;
    if !head.success {
        return Err(StoreError::Corrupt(format!("the run branch {} has no commit", record.branch)));
    }
    // Where the local base branch is now: the run was rebased onto the fetched target, which the
    // local branch may be behind, and only this commit is fast-forwarded from.
    let base = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.target_branch)])?;
    if !base.success {
        return Err(StoreError::Corrupt(format!("the base branch {} has no commit", record.target_branch)));
    }
    Ok((head.stdout.trim().to_string(), base.stdout.trim().to_string()))
}

/// The files left with conflict markers in `checkout`, escaped for showing.
fn unmerged_paths(checkout: &Path) -> Result<Vec<String>, StoreError> {
    let args = ["diff", "--name-only", "-z", "--diff-filter=U"].map(OsStr::new);
    let diff = git_bytes(checkout, &args)?;
    let mut paths: Vec<String> =
        diff.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()).map(|path| printable(&String::from_utf8_lossy(path))).collect();
    paths.dedup();
    Ok(paths)
}

fn stash_listing(checkout: &Path) -> Result<String, StoreError> {
    let listed = git_raw(checkout, &["stash", "list", "--format=%H %gs"])?;
    if !listed.success {
        return Err(StoreError::Corrupt(format!("cannot list the stash at {}: {}", checkout.display(), listed.stderr.trim())));
    }
    Ok(listed.stdout)
}

/// Drop the entry saved with `message`, found again where it is now: the stack is shared, and
/// other entries may have been pushed on top of it.
fn drop_stash(checkout: &Path, message: &str, stash: &str) -> Result<(), StoreError> {
    if let Some((index, sha)) = stash_entry(&stash_listing(checkout)?, message) {
        if sha == stash {
            git_raw(checkout, &["stash", "drop", "--quiet", &format!("stash@{{{index}}}")])?;
        }
    }
    Ok(())
}

/// How many overlapping files a refusal names before it only counts the rest.
const NAMED_FILES: usize = 5;

/// `paths` for a reason: the first few by name, then how many more there are.
fn named(paths: &[String]) -> String {
    let shown = paths.iter().take(NAMED_FILES).map(|path| path.as_str()).collect::<Vec<_>>().join(", ");
    match paths.len().saturating_sub(NAMED_FILES) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// The files the commits from `base_sha` to `head_sha` change, or why they could not be read.
fn changed_paths(worktree: &Path, base_sha: &str, head_sha: &str) -> Result<Result<BTreeSet<PathBuf>, String>, StoreError> {
    // Without rename detection a renamed file is listed under both its paths.
    let args = ["diff", "--name-only", "-z", "--no-renames", base_sha, head_sha].map(OsStr::new);
    let diff = git_bytes(worktree, &args)?;
    if !diff.status.success() {
        return Ok(Err(format!("cannot read the files this run changes: {}", String::from_utf8_lossy(&diff.stderr).trim())));
    }
    Ok(Ok(diff.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()).map(path_of).collect()))
}

/// The tracked files with uncommitted changes in `checkout`, staged or not, or why they could not
/// be read.
fn dirty_paths(checkout: &Path) -> Result<Result<BTreeSet<PathBuf>, String>, StoreError> {
    let args = ["status", "--porcelain=v1", "-z", "--untracked-files=no"].map(OsStr::new);
    let status = git_bytes(checkout, &args)?;
    if !status.status.success() {
        return Ok(Err(unreadable(checkout, &String::from_utf8_lossy(&status.stderr))));
    }
    let mut dirty = BTreeSet::new();
    let mut entries = status.stdout.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else { continue };
        dirty.insert(path_of(path));
        // A rename or copy is followed by the path it came from, as an entry of its own.
        if code.iter().any(|byte| matches!(byte, b'R' | b'C')) {
            if let Some(from) = entries.next() {
                dirty.insert(path_of(from));
            }
        }
    }
    Ok(Ok(dirty))
}

/// A path as git printed it, byte for byte where the platform allows, so two names that differ
/// only in bytes that are not UTF-8 stay apart.
fn path_of(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn unreadable(checkout: &Path, why: &str) -> String {
    format!("cannot read the uncommitted changes at {}: {}", checkout.display(), why.trim())
}

/// What a dirty path holds in the working tree: a regular file by its blob id, a symbolic link by
/// where it points, anything else (gone, a directory) as nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    File(String),
    Link(PathBuf),
    Absent,
}

/// `paths` in `checkout` with their contents, or why git could not hash them.
fn contents(checkout: &Path, paths: BTreeSet<PathBuf>) -> Result<Result<BTreeMap<PathBuf, Content>, String>, StoreError> {
    let mut read: BTreeMap<PathBuf, Content> = BTreeMap::new();
    let mut files = Vec::new();
    // Gone, or a file where a directory on its path should be: either way nothing is there.
    let absent = |err: &std::io::Error| matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory);
    for path in paths {
        let content = match std::fs::symlink_metadata(checkout.join(&path)) {
            Ok(meta) if meta.is_file() => {
                files.push(path);
                continue;
            }
            Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(checkout.join(&path)) {
                Ok(target) => Content::Link(target),
                Err(err) if absent(&err) => Content::Absent,
                Err(err) => return Ok(Err(unreadable(checkout, &format!("{}: {err}", path.display())))),
            },
            Ok(_) => Content::Absent,
            Err(err) if absent(&err) => Content::Absent,
            Err(err) => return Ok(Err(unreadable(checkout, &format!("{}: {err}", path.display())))),
        };
        read.insert(path, content);
    }
    // In batches, so a checkout with many changed files stays within the argument limit.
    for batch in files.chunks(500) {
        let mut args = vec![OsStr::new("hash-object"), OsStr::new("--")];
        args.extend(batch.iter().map(|path| path.as_os_str()));
        let hashed = git_bytes(checkout, &args)?;
        if !hashed.status.success() {
            return Ok(Err(unreadable(checkout, &String::from_utf8_lossy(&hashed.stderr))));
        }
        let stdout = String::from_utf8_lossy(&hashed.stdout);
        let ids: Vec<&str> = stdout.lines().collect();
        if ids.len() != batch.len() {
            return Ok(Err(unreadable(checkout, &format!("git hashed {} of {} files", ids.len(), batch.len()))));
        }
        read.extend(batch.iter().zip(ids).map(|(path, id)| (path.clone(), Content::File(id.to_string()))));
    }
    Ok(Ok(read))
}

/// Where the base branch is now, when a fast-forward from `base_sha` failed because it moved on
/// meanwhile; `None` when it is still there and the failure has another cause.
fn moved_on_from(record: &RunRecord, base_sha: &str) -> Result<Option<String>, StoreError> {
    let now = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.target_branch)])?;
    let now = now.stdout.trim();
    Ok((!now.is_empty() && now != base_sha).then(|| now.to_string()))
}

/// The working tree that has the target branch checked out, if any does.
fn checkout_of(record: &RunRecord) -> Result<Option<PathBuf>, StoreError> {
    // NUL-separated, so a path is read whole whatever characters it holds.
    let listed = git_raw(&record.worktree, &["worktree", "list", "--porcelain", "-z"])?;
    if !listed.success {
        return Err(StoreError::Corrupt(format!("cannot list the repository's worktrees: {}", listed.stderr.trim())));
    }
    let wanted = format!("branch refs/heads/{}", record.target_branch);
    Ok(listed.stdout.split("\0\0").find_map(|block| {
        let mut fields = block.split('\0');
        let path = fields.next()?.strip_prefix("worktree ")?;
        fields.any(|field| field == wanted).then(|| PathBuf::from(path))
    }))
}

/// Where the stash entry saved with `message` is in `listing` (`git stash list --format='%H %gs'`
/// output), and its commit: other entries may have been pushed on top of it meanwhile.
fn stash_entry(listing: &str, message: &str) -> Option<(usize, String)> {
    listing.lines().enumerate().find_map(|(index, line)| {
        let (sha, subject) = line.split_once(' ')?;
        let (_, saved) = subject.split_once(": ")?;
        (saved == message).then(|| (index, sha.to_string()))
    })
}

fn record_event(store: &RunStore, record: &RunRecord, kind: &str, payload: serde_json::Value) -> Result<(), StoreError> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-merge-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_dirty_path_that_cannot_be_examined_is_refused() {
        let checkout = scratch("unexaminable");
        // `d` points at itself, so looking up `d/f` fails with "too many levels of symbolic links"
        // rather than "not found", whoever runs the test.
        std::os::unix::fs::symlink("d", checkout.join("d")).unwrap();
        let read = contents(&checkout, BTreeSet::from([PathBuf::from("d/f")])).unwrap();
        let why = read.expect_err("a path that cannot be examined must refuse");
        assert!(why.starts_with(&format!("cannot read the uncommitted changes at {}: ", checkout.display())), "{why}");
        assert!(why.contains("d/f"), "{why}");
        let _ = std::fs::remove_dir_all(&checkout);
    }

    #[test]
    fn a_dirty_path_that_is_gone_or_under_a_file_counts_as_absent() {
        let checkout = scratch("absent");
        std::fs::write(checkout.join("plain"), "x").unwrap();
        let read = contents(&checkout, BTreeSet::from([PathBuf::from("gone"), PathBuf::from("plain/f")])).unwrap().unwrap();
        assert_eq!(read.get(Path::new("gone")), Some(&Content::Absent));
        assert_eq!(read.get(Path::new("plain/f")), Some(&Content::Absent));
        let _ = std::fs::remove_dir_all(&checkout);
    }

    #[test]
    fn a_stash_entry_is_found_by_its_message_wherever_it_is_on_the_stack() {
        let listing = format!(
            "{top} On main: someone else's\n{ours} On main: orq set-aside run-1 2026-11-15T00:00:00Z\n{older} WIP on main: abc base\n",
            top = "1".repeat(40),
            ours = "2".repeat(40),
            older = "3".repeat(40)
        );
        assert_eq!(stash_entry(&listing, "orq set-aside run-1 2026-11-15T00:00:00Z"), Some((1, "2".repeat(40))));
        assert_eq!(stash_entry(&listing, "orq set-aside run-2 2026-11-15T00:00:00Z"), None);
    }
}
