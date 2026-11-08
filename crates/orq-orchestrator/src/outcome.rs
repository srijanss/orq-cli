//! After the implement session: is there something to push? A worktree with
//! uncommitted changes, or one with no commits past the base, blocks the run.
//! The worktree is never touched, so a person can finish the work by hand.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::implement::{next_event_id, now_rfc3339};
use crate::store::{Event, RunStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplementCheck {
    /// Clean, with this many commits past the run's base.
    Ready { commits: u32 },
    /// The run stops here: no push, review request or CI wait follows.
    Blocked { reason: String },
}

pub fn check_implement_outcome(
    store: &RunStore,
    run_id: &str,
) -> Result<ImplementCheck, StoreError> {
    let record = store.load(run_id)?;
    let worktree = record.worktree.as_path();
    let dirty = git(
        worktree,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    // The history is only walked for a clean worktree: a dirty one is blocked
    // whatever the commits are.
    let (reason, commits) = if !dirty.trim().is_empty() {
        (Some("the worktree has uncommitted changes".to_string()), 0)
    } else {
        let count = git(
            worktree,
            &["rev-list", "--count", &format!("{}..HEAD", record.base_sha)],
        )?;
        let commits: u32 = count
            .trim()
            .parse()
            .map_err(|_| StoreError::Corrupt(format!("unexpected commit count {count:?}")))?;
        (
            if commits == 0 {
                Some("the implementer made no new commits".to_string())
            } else {
                None
            },
            commits,
        )
    };
    let Some(reason) = reason else {
        return Ok(ImplementCheck::Ready { commits });
    };
    store.append_event(&Event {
        schema_version: 1,
        event_id: next_event_id(run_id),
        run_id: record.run_id.clone(),
        project_id: record.project_id.clone(),
        feature_id: record.feature_id.clone(),
        kind: "run.blocked".to_string(),
        stage: Some("implement_and_review".to_string()),
        attempt: 1,
        occurred_at: now_rfc3339(),
        payload: serde_json::json!({ "reason": reason }),
    })?;
    Ok(ImplementCheck::Blocked { reason })
}

fn git(worktree: &Path, args: &[&str]) -> Result<String, StoreError> {
    let out = git_raw(worktree, args)?;
    if !out.success {
        return Err(StoreError::Corrupt(format!(
            "git {} failed: {}",
            args[0],
            out.stderr.trim()
        )));
    }
    Ok(out.stdout)
}

pub(crate) struct GitOutput {
    pub success: bool,
    /// The exit code, or `None` when git was ended by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run git in `worktree` without the caller's repository-selection variables;
/// a non-zero exit is reported in the result, not as an error.
pub(crate) fn git_raw(worktree: &Path, args: &[&str]) -> Result<GitOutput, StoreError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(worktree).args(args);
    run_sanitized(command)
}

/// Run an already built git command with the repository-selection variables removed and
/// prompts disabled, so the caller's environment cannot pick the repository or its config.
pub(crate) fn run_sanitized(mut command: Command) -> Result<GitOutput, StoreError> {
    // Same list as orq-git: never let the caller's environment pick the repository.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    // Never block on a credential or editor prompt.
    command.envs([("GIT_TERMINAL_PROMPT", "0"), ("GIT_EDITOR", "true")]);
    let out = command.stdin(Stdio::null()).output()?;
    Ok(GitOutput {
        success: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}
