//! Fetching a run's target branch and recording the exact commit it points
//! at. Only remote-tracking refs are written: the checkout's HEAD, index and
//! working tree are never used or touched.

use std::path::Path;
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// A remote or branch name that could be read as an option, a refspec or
    /// more than one argument.
    InvalidName { what: &'static str, value: String },
    RemoteNotFound { remote: String },
    BranchNotFound { remote: String, branch: String },
    /// Git could not be run, or failed for another reason (network, not a
    /// repository, ...).
    Git { message: String },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::InvalidName { what, value } => write!(f, "invalid {what} name {value:?}"),
            FetchError::RemoteNotFound { remote } => write!(f, "remote {remote:?} does not exist"),
            FetchError::BranchNotFound { remote, branch } => {
                write!(f, "branch {branch:?} does not exist on remote {remote:?}")
            }
            FetchError::Git { message } => write!(f, "git failed: {message}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Verify `remote/branch` exists, fetch it, and return the SHA of the
/// resulting remote-tracking commit.
pub fn fetch_target_base(repo: &Path, remote: &str, branch: &str) -> Result<String, FetchError> {
    check_remote(remote)?;
    check_branch(repo, branch)?;

    let url = git(repo, &["remote", "get-url", "--", remote])?;
    if !url.status.success() {
        return Err(match url.status.code() {
            Some(2) => FetchError::RemoteNotFound { remote: remote.to_string() },
            _ => failure(&url),
        });
    }

    let head_ref = format!("refs/heads/{branch}");
    let listed = git(repo, &["ls-remote", "--exit-code", "--heads", "--", remote, &head_ref])?;
    match listed.status.code() {
        Some(0) => {}
        Some(2) => {
            return Err(FetchError::BranchNotFound { remote: remote.to_string(), branch: branch.to_string() })
        }
        _ => return Err(failure(&listed)),
    }

    let tracking = format!("refs/remotes/{remote}/{branch}");
    let refspec = format!("+{head_ref}:{tracking}");
    let fetched = git(repo, &["fetch", "--quiet", "--no-tags", "--refmap=", "--", remote, &refspec])?;
    if !fetched.status.success() {
        return Err(failure(&fetched));
    }

    let parsed = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{tracking}^{{commit}}")])?;
    if !parsed.status.success() {
        return Err(failure(&parsed));
    }
    let sha = String::from_utf8_lossy(&parsed.stdout).trim().to_string();
    if sha.len() < 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(FetchError::Git { message: format!("unexpected rev-parse output {sha:?}") });
    }
    Ok(sha)
}

fn check_remote(remote: &str) -> Result<(), FetchError> {
    let ok = !remote.is_empty()
        && !remote.starts_with('-')
        && remote.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if ok {
        Ok(())
    } else {
        Err(FetchError::InvalidName { what: "remote", value: remote.to_string() })
    }
}

fn check_branch(repo: &Path, branch: &str) -> Result<(), FetchError> {
    let invalid = || FetchError::InvalidName { what: "branch", value: branch.to_string() };
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid());
    }
    // `check-ref-format` rejects `..`, `:`, `~`, `^`, `?`, `*`, `[`, `\\` and the rest.
    let checked = git(repo, &["check-ref-format", &format!("refs/heads/{branch}")])?;
    match checked.status.code() {
        Some(0) => Ok(()),
        // check-ref-format exits 1 for a bad name; anything else is git itself failing.
        Some(1) => Err(invalid()),
        _ => Err(failure(&checked)),
    }
}

fn failure(output: &Output) -> FetchError {
    FetchError::Git { message: redact_urls(String::from_utf8_lossy(&output.stderr).trim()) }
}

/// Drop credentials (`user:token@`), query strings and fragments from every
/// URL in git's diagnostics, which may echo a remote's configured URL.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // Quotes and apostrophes are valid inside userinfo and query values, so
        // only whitespace ends a URL; over-redacting is the safe direction.
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        // Query and fragment go first, so an '@' inside them is never read as
        // userinfo; then userinfo is dropped from the authority.
        let cut = url.find(['?', '#']).unwrap_or(url.len());
        let url = &url[..cut];
        let authority_end = url.find('/').unwrap_or(url.len());
        let (authority, path) = url.split_at(authority_end);
        out.push_str(authority.rsplit('@').next().unwrap_or(authority));
        out.push_str(path);
        rest = after;
    }
    out.push_str(rest);
    out
}

fn git(repo: &Path, args: &[&str]) -> Result<Output, FetchError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    for name in crate::REPOSITORY_SELECTION_VARS {
        command.env_remove(name);
    }
    command
        // A missing credential fails the fetch instead of waiting on a prompt.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| FetchError::Git { message: err.to_string() })
}
