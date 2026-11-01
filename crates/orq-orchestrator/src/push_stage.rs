//! The push stage: publish the run branch to the selected remote. Only the
//! run's own branch is pushed, never forced and never the target branch; the
//! commit that was pushed is recorded for the review request and CI to refer to.

use std::io::Write;
use std::path::Path;

use crate::implement::{next_event_id, now_rfc3339, open_log};
use crate::outcome::git_raw;
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "push";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The remote now has the run branch at this commit.
    Pushed { head_sha: String },
    /// Nothing was published, or git refused; the run's stage has failed.
    Failed { reason: String },
}

/// Push the run branch of `run_id`, logging git's output to `log_path`.
pub fn push_run_branch(store: &RunStore, run_id: &str, log_path: &Path) -> Result<PushOutcome, StoreError> {
    let record = store.load(run_id)?;
    let log_name = log_path.to_string_lossy().into_owned();
    if record.branch == record.target_branch {
        let reason = format!("the run branch {} is the target branch and is never pushed", record.branch);
        return fail(store, &record, reason, &log_name);
    }
    let worktree = record.worktree.as_path();
    // Only the run's own branch is published: a switched or detached HEAD is not.
    let head = git_raw(worktree, &["symbolic-ref", "-q", "HEAD"])?;
    if head.stdout.trim() != format!("refs/heads/{}", record.branch) {
        return Err(StoreError::Corrupt(format!("the worktree is not on the run branch {}", record.branch)));
    }
    let head_sha = git_raw(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if !head_sha.success {
        return Err(StoreError::Corrupt("the run branch has no commit".to_string()));
    }
    let head_sha = head_sha.stdout.trim().to_string();

    let mut log = open_log(log_path)?;
    record_event(store, &record, "stage.started", serde_json::json!({ "log": log_name, "branch": record.branch }))?;
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", record.branch);
    writeln!(log, "$ {}", redact_credentials(&format!("git push --no-follow-tags -- {} {refspec}", record.remote)))?;
    // No force option: a remote branch that diverged is reported, not overwritten.
    // `push.followTags` in the user's config must not publish tags with it.
    let pushed = git_raw(worktree, &["push", "--no-follow-tags", "--", &record.remote, &refspec])?;
    log.write_all(redact_credentials(&pushed.stdout).as_bytes())?;
    log.write_all(redact_credentials(&pushed.stderr).as_bytes())?;
    if !pushed.success {
        let reason = format!("git push of {} failed", record.branch);
        return fail(store, &record, reason, &log_name);
    }
    record_event(
        store,
        &record,
        "stage.completed",
        serde_json::json!({ "pushed_sha": head_sha, "branch": record.branch, "log": log_name }),
    )?;
    Ok(PushOutcome::Pushed { head_sha })
}

/// Drop the `user:password@` part of every URL in `text`: a remote may carry a
/// token, and the log outlives the run.
fn redact_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || c == '/' || c == '\'' || c == '"').unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(authority.rsplit_once('@').map_or(authority, |(_, host)| host));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn fail(store: &RunStore, record: &RunRecord, reason: String, log_name: &str) -> Result<PushOutcome, StoreError> {
    record_event(store, record, "stage.failed", serde_json::json!({ "reason": reason, "log": log_name }))?;
    Ok(PushOutcome::Failed { reason })
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
