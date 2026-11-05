//! The push stage: publish the run branch to the selected remote. Only the
//! run's own branch is pushed, never forced and never the target branch; the
//! commit that was pushed is recorded for the review request and CI to refer to.

use std::io::Write;
use std::path::Path;

use crate::implement::{next_event_id, now_rfc3339, open_log};
use crate::outcome::git_raw;
use crate::review_request::checked_push_url;
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
pub fn push_run_branch(
    store: &RunStore,
    run_id: &str,
    log_path: &Path,
) -> Result<PushOutcome, StoreError> {
    let record = store.load(run_id)?;
    let log_name = log_path.to_string_lossy().into_owned();
    if record.branch == record.target_branch {
        let reason = format!(
            "the run branch {} is the target branch and is never pushed",
            record.branch
        );
        return fail(store, &record, reason, &log_name);
    }
    // The human-only merge rule: a push may never update a protected branch, whatever the target is.
    let protected = [
        record.target_branch.clone(),
        "main".to_string(),
        "master".to_string(),
    ];
    let push_args = [
        "push".to_string(),
        record.remote.clone(),
        format!("HEAD:refs/heads/{}", record.branch),
    ];
    if let Some(why) = orq_core::no_merge::forbidden_git_push(&push_args, &protected) {
        return fail(
            store,
            &record,
            format!("the run branch {} is refused: {why}", record.branch),
            &log_name,
        );
    }
    let worktree = record.worktree.as_path();
    // Only the run's own branch is published: a switched or detached HEAD is not.
    let head = git_raw(worktree, &["symbolic-ref", "-q", "HEAD"])?;
    if head.stdout.trim() != format!("refs/heads/{}", record.branch) {
        return Err(StoreError::Corrupt(format!(
            "the worktree is not on the run branch {}",
            record.branch
        )));
    }
    let head_sha = git_raw(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if !head_sha.success {
        return Err(StoreError::Corrupt(
            "the run branch has no commit".to_string(),
        ));
    }
    let head_sha = head_sha.stdout.trim().to_string();
    // The session can change the remote's configuration: a pinned run pushes to the url it just
    // checked, not to the name git would look up again.
    let checked = checked_push_url(&record)?;
    let destination = checked.clone().unwrap_or_else(|| record.remote.clone());

    let mut log = open_log(log_path)?;
    record_event(
        store,
        &record,
        "stage.started",
        serde_json::json!({ "log": log_name, "branch": record.branch }),
    )?;
    // The source is the commit that was recorded, so a branch moved meanwhile cannot change what is published.
    let refspec = format!("{head_sha}:refs/heads/{}", record.branch);
    writeln!(
        log,
        "$ {}",
        redact_credentials(&format!(
            "git push --no-follow-tags -- {destination} {refspec}"
        ))
    )?;
    // No force option: a remote branch that diverged is reported, not overwritten.
    // `push.followTags` in the user's config must not publish tags with it.
    let pushed = if checked.is_some() {
        // A setup failure is a failed stage, not an error that leaves the stage started.
        match isolated_push(worktree, &destination, &refspec) {
            Ok(pushed) => pushed,
            Err(error) => {
                writeln!(log, "{}", redact_credentials(&error.to_string()))?;
                return fail(
                    store,
                    &record,
                    format!("git push of {} failed", record.branch),
                    &log_name,
                );
            }
        }
    } else {
        git_raw(
            worktree,
            &["push", "--no-follow-tags", "--", &destination, &refspec],
        )?
    };
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

/// Push `refspec` to the already checked `url` from an empty scratch repository that borrows the
/// worktree's objects. The worktree's own configuration (url rewrites, hooks, pushurl) is never
/// read, so it cannot change where the commit goes between the check and the push.
fn isolated_push(
    worktree: &Path,
    url: &str,
    refspec: &str,
) -> Result<crate::outcome::GitOutput, StoreError> {
    let common = git_raw(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !common.success {
        return Err(StoreError::Corrupt(
            "the worktree has no git directory".to_string(),
        ));
    }
    let objects = Path::new(common.stdout.trim()).join("objects");
    let format = git_raw(worktree, &["rev-parse", "--show-object-format"])?;
    let format = if format.success {
        format.stdout.trim().to_string()
    } else {
        "sha1".to_string()
    };
    let scratch = std::env::temp_dir().join(format!(
        "orq-push-{}-{}",
        std::process::id(),
        next_event_id("scratch")
    ));
    std::fs::create_dir_all(&scratch)?;
    let result = (|| {
        let init = git_raw(
            &scratch,
            &["init", "-q", &format!("--object-format={format}")],
        )?;
        if !init.success {
            return Err(StoreError::Corrupt(format!(
                "could not create a scratch repository for the push: {}",
                init.stderr.trim()
            )));
        }
        // An alternates file holds one path per line, so a ':' in the checkout path is safe
        // (GIT_ALTERNATE_OBJECT_DIRECTORIES would split on it).
        let alternates = scratch
            .join(".git")
            .join("objects")
            .join("info")
            .join("alternates");
        std::fs::write(&alternates, format!("{}\n", alternate_line(&objects)))?;
        let mut command = std::process::Command::new("git");
        command
            .arg("-C")
            .arg(&scratch)
            .args(["push", "--no-follow-tags", "--", url, refspec]);
        crate::outcome::run_sanitized(command)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// One line of an alternates file for `path`: git reads a line starting with a double quote as a
/// C-style quoted path, which is how a newline (or a leading quote) in the path survives.
fn alternate_line(path: &Path) -> String {
    let text = path.to_string_lossy();
    if !text.contains(['\n', '"', '\\']) && !text.starts_with('"') {
        return text.into_owned();
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Drop the `user:password@` part of every URL in `text`: a remote may carry a
/// token, and the log outlives the run.
fn redact_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // Quotes are valid in userinfo, so only whitespace and the path end the authority;
        // a quote closing the URL then stays with the host.
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn fail(
    store: &RunStore,
    record: &RunRecord,
    reason: String,
    log_name: &str,
) -> Result<PushOutcome, StoreError> {
    record_event(
        store,
        record,
        "stage.failed",
        serde_json::json!({ "reason": reason, "log": log_name }),
    )?;
    Ok(PushOutcome::Failed { reason })
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
