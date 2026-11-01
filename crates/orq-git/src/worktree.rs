//! Creating a run's worktree and branch from a recorded base commit.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    /// A branch name, commit id or worktree directory that is not safe to use.
    InvalidName { what: &'static str, value: String },
    BranchExists { branch: String },
    PathExists { path: PathBuf },
    Git { message: String },
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorktreeError::InvalidName { what, value } => write!(f, "invalid {what} {value:?}"),
            WorktreeError::BranchExists { branch } => write!(f, "branch {branch:?} already exists"),
            WorktreeError::PathExists { path } => write!(f, "{} already exists", path.display()),
            WorktreeError::Git { message } => write!(f, "git failed: {message}"),
        }
    }
}

impl std::error::Error for WorktreeError {}

/// Create `repo/<worktree_dir>` on a new branch `branch` that starts at
/// `base_sha`, whatever the source checkout has checked out. Only the new
/// branch and worktree are written: the source checkout's HEAD, index and
/// working tree are not touched. A taken branch or an existing path is
/// refused, never adopted.
pub fn create_run_worktree(
    repo: &Path,
    worktree_dir: &Path,
    branch: &str,
    base_sha: &str,
) -> Result<PathBuf, WorktreeError> {
    let invalid = |what: &'static str, value: &str| WorktreeError::InvalidName { what, value: value.to_string() };
    // A full object id, so it can never be read as an option or a ref name.
    if !matches!(base_sha.len(), 40 | 64) || !base_sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid("base commit", base_sha));
    }
    // Relative, and plain components only: no `..`, root or prefix.
    let plain = !worktree_dir.as_os_str().is_empty()
        && worktree_dir.components().all(|c| matches!(c, Component::Normal(_)));
    if !plain {
        return Err(invalid("worktree directory", &worktree_dir.to_string_lossy()));
    }
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("branch", branch));
    }
    let head_ref = format!("refs/heads/{branch}");
    let checked = git(repo, &["check-ref-format", &head_ref])?;
    match checked.status.code() {
        Some(0) => {}
        Some(1) => return Err(invalid("branch", branch)),
        _ => return Err(failure(&checked)),
    }

    let path = repo.join(worktree_dir);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(WorktreeError::PathExists { path });
    }
    let existing = git(repo, &["show-ref", "--verify", "--quiet", &head_ref])?;
    match existing.status.code() {
        Some(0) => return Err(WorktreeError::BranchExists { branch: branch.to_string() }),
        Some(1) => {}
        _ => return Err(failure(&existing)),
    }
    let commit = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{base_sha}^{{commit}}")])?;
    if !commit.status.success() {
        return Err(WorktreeError::Git { message: format!("base commit {base_sha} is not in the repository") });
    }

    let path_arg = path.to_string_lossy().into_owned();
    let added = git(repo, &["worktree", "add", "--quiet", "--no-track", "-b", branch, "--", &path_arg, base_sha])?;
    if !added.status.success() {
        return Err(failure(&added));
    }
    Ok(path)
}

fn failure(output: &Output) -> WorktreeError {
    WorktreeError::Git { message: crate::redact_urls(String::from_utf8_lossy(&output.stderr).trim()) }
}

fn git(repo: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    for name in crate::REPOSITORY_SELECTION_VARS {
        command.env_remove(name);
    }
    command
        .stdin(Stdio::null())
        .output()
        .map_err(|err| WorktreeError::Git { message: err.to_string() })
}
