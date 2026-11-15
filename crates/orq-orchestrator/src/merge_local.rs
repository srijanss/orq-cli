//! A merge-local run delivers its work without a remote: the base branch of the run's own
//! repository is fast-forwarded to the run's branch. Never a merge commit, never a push.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::cleanup::printable;
use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
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
    let head = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", record.branch)])?;
    if !head.success {
        return Err(StoreError::Corrupt(format!("the run branch {} has no commit", record.branch)));
    }
    let head_sha = head.stdout.trim().to_string();
    let target = format!("refs/heads/{}", record.target_branch);
    // Where the local base branch is now: the run was rebased onto the fetched target, which the
    // local branch may be behind, and only this commit is fast-forwarded from.
    let base = git_raw(&record.worktree, &["rev-parse", "--verify", &format!("{target}^{{commit}}")])?;
    if !base.success {
        return Err(StoreError::Corrupt(format!("the base branch {} has no commit", record.target_branch)));
    }
    let base_sha = base.stdout.trim().to_string();
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
            let overlapping: Vec<String> = dirty.keys().filter(|path| changed.contains(*path)).map(|path| printable(path)).collect();
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
                        Ok(now) => now,
                        Err(reason) => return Ok(MergeOutcome::Refused { reason }),
                    };
                    if now.iter().any(|(path, content)| dirty.get(path) != Some(content)) {
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
fn changed_paths(worktree: &Path, base_sha: &str, head_sha: &str) -> Result<Result<BTreeSet<String>, String>, StoreError> {
    // Without rename detection a renamed file is listed under both its paths.
    let diff = git_raw(worktree, &["diff", "--name-only", "-z", "--no-renames", base_sha, head_sha])?;
    if !diff.success {
        return Ok(Err(format!("cannot read the files this run changes: {}", diff.stderr.trim())));
    }
    Ok(Ok(diff.stdout.split('\0').filter(|path| !path.is_empty()).map(str::to_string).collect()))
}

/// The tracked files with uncommitted changes in `checkout`, staged or not, each with what it holds
/// now (see [`contents`]), or why they could not be read.
fn dirty_paths(checkout: &Path) -> Result<Result<BTreeMap<String, Content>, String>, StoreError> {
    let status = git_raw(checkout, &["status", "--porcelain=v1", "-z", "--untracked-files=no"])?;
    if !status.success {
        return Ok(Err(format!("cannot read the uncommitted changes at {}: {}", checkout.display(), status.stderr.trim())));
    }
    let mut dirty = BTreeSet::new();
    let mut entries = status.stdout.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else { continue };
        dirty.insert(path.to_string());
        // A rename or copy is followed by the path it came from, as an entry of its own.
        if code.contains(['R', 'C']) {
            if let Some(from) = entries.next() {
                dirty.insert(from.to_string());
            }
        }
    }
    contents(checkout, dirty)
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
fn contents(checkout: &Path, paths: BTreeSet<String>) -> Result<Result<BTreeMap<String, Content>, String>, StoreError> {
    let mut read: BTreeMap<String, Content> = BTreeMap::new();
    let mut files = Vec::new();
    for path in paths {
        match std::fs::symlink_metadata(checkout.join(&path)) {
            Ok(meta) if meta.is_file() => files.push(path),
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(checkout.join(&path)).unwrap_or_default();
                read.insert(path, Content::Link(target));
            }
            _ => {
                read.insert(path, Content::Absent);
            }
        }
    }
    // In batches, so a checkout with many changed files stays within the argument limit.
    for batch in files.chunks(500) {
        let mut args = vec!["hash-object", "--"];
        args.extend(batch.iter().map(String::as_str));
        let hashed = git_raw(checkout, &args)?;
        if !hashed.success {
            return Ok(Err(format!("cannot read the uncommitted changes at {}: {}", checkout.display(), hashed.stderr.trim())));
        }
        let ids: Vec<&str> = hashed.stdout.lines().collect();
        if ids.len() != batch.len() {
            return Ok(Err(format!("cannot read the uncommitted changes at {}: git hashed {} of {} files", checkout.display(), ids.len(), batch.len())));
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
