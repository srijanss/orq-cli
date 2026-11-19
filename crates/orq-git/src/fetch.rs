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
    /// The project root has no local branch of that name to start from.
    LocalBranchNotFound { branch: String },
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
            FetchError::LocalBranchNotFound { branch } => {
                write!(f, "branch {branch:?} does not exist in the project root; create it or check it out first")
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

/// The SHA of the local `branch` in `repo`, the commit a run starts from. Nothing is fetched, and
/// the checkout's HEAD, index and working tree are never used or touched.
pub fn local_target_base(repo: &Path, branch: &str) -> Result<String, FetchError> {
    check_branch(repo, branch)?;
    let parsed = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}^{{commit}}")])?;
    if !parsed.status.success() {
        return Err(match parsed.status.code() {
            Some(1) => FetchError::LocalBranchNotFound { branch: branch.to_string() },
            _ => failure(&parsed),
        });
    }
    let sha = String::from_utf8_lossy(&parsed.stdout).trim().to_string();
    if sha.len() < 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(FetchError::Git { message: format!("unexpected rev-parse output {sha:?}") });
    }
    Ok(sha)
}

/// What [`pull_target`] did to a project's local target branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    /// The local target was behind the remote's and was moved to it, fast-forward only.
    FastForwarded { from: String, to: String },
    /// The local target is the remote's: nothing to do.
    UpToDate,
    /// The local target has `n` commits the remote's lacks, and none the other way: nothing to do.
    Ahead { n: usize },
    /// Both sides have commits the other lacks: nothing was touched.
    Diverged { local_only: usize, remote_only: usize },
    /// The checkout is on the target and has changes to these tracked files (at most
    /// [`DIRTY_FILES_NAMED`]): nothing was touched.
    Dirty { files: Vec<String> },
    /// Git refused the fast-forward, for this reason (an untracked file it would overwrite, ...):
    /// nothing was touched.
    Refused { reason: String },
    /// The remote's target could not be fetched, for this reason with any credentials in a URL
    /// removed: nothing was touched.
    FetchFailed { message: String },
    /// The project has no such remote, so there is nothing to pull: nothing was touched.
    NoRemote,
}

impl PullOutcome {
    /// The outcome's name, as events and the API carry it.
    pub fn kind(&self) -> &'static str {
        match self {
            PullOutcome::FastForwarded { .. } => "fast_forwarded",
            PullOutcome::UpToDate => "up_to_date",
            PullOutcome::Ahead { .. } => "ahead",
            PullOutcome::Diverged { .. } => "diverged",
            PullOutcome::Dirty { .. } => "dirty",
            PullOutcome::Refused { .. } => "refused",
            PullOutcome::FetchFailed { .. } => "fetch_failed",
            PullOutcome::NoRemote => "no_remote",
        }
    }

    /// Whether a run may start from the local target now: it holds every commit `remote` has, or
    /// there is no `remote` to have any.
    pub fn may_start(&self) -> bool {
        matches!(self, PullOutcome::FastForwarded { .. } | PullOutcome::UpToDate | PullOutcome::Ahead { .. } | PullOutcome::NoRemote)
    }

    /// What happened to `target`, pulled from `remote`, in a line for a person.
    pub fn describe(&self, remote: &str, target: &str) -> String {
        let plural = |n: usize| if n == 1 { "commit" } else { "commits" };
        match self {
            PullOutcome::FastForwarded { from, to } => format!("{target} fast-forwarded from {} to {}", short(from), short(to)),
            PullOutcome::UpToDate => format!("{target} is up to date with {remote}"),
            PullOutcome::Ahead { n } => format!("{target} is {n} {} ahead of {remote}", plural(*n)),
            PullOutcome::Diverged { local_only, remote_only } => {
                format!("{target} has {local_only} {} not on {remote} and {remote} has {remote_only} not here", plural(*local_only))
            }
            PullOutcome::Dirty { files } => format!("{target} has uncommitted changes to {}", files.join(", ")),
            PullOutcome::Refused { reason } => format!("{target} could not be fast-forwarded: {reason}"),
            PullOutcome::FetchFailed { message } => format!("{target} could not be fetched from {remote}: {message}"),
            PullOutcome::NoRemote => format!("{target} has no {remote} to be pulled from"),
        }
    }
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// How many changed files a [`PullOutcome::Dirty`] names.
pub const DIRTY_FILES_NAMED: usize = 10;

/// Fetch `remote/target` into `repo` (the project's main checkout) and bring the local `target` up
/// to it, fast-forward only. Never merges, rebases, resets, stashes or pushes.
pub fn pull_target(repo: &Path, remote: &str, target: &str) -> PullOutcome {
    let to = match fetch_target_base(repo, remote, target) {
        Ok(to) => to,
        // A project without the remote (one never pushed anywhere) has nothing to bring its target level with.
        Err(FetchError::RemoteNotFound { .. }) => return PullOutcome::NoRemote,
        Err(error) => return PullOutcome::FetchFailed { message: redact_urls(&error.to_string()) },
    };
    match fast_forward(repo, target, &to) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

/// Brings the local `target` of `repo` up to the fetched commit `to`, fast-forward only.
fn fast_forward(repo: &Path, target: &str, to: &str) -> Result<PullOutcome, PullOutcome> {
    let refused = |reason: String| PullOutcome::Refused { reason: redact_urls(&reason) };
    let from = local_target_base(repo, target).map_err(|error| refused(error.to_string()))?;
    if from == to {
        return Ok(PullOutcome::UpToDate);
    }
    let counted = git(repo, &["rev-list", "--left-right", "--count", &format!("{from}...{to}"), "--"]).map_err(|error| refused(error.to_string()))?;
    if !counted.status.success() {
        return Err(refused(stderr(&counted)));
    }
    let counts: Vec<usize> = String::from_utf8_lossy(&counted.stdout).split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let [local_only, remote_only] = counts[..] else { return Err(refused(format!("unexpected rev-list output {counts:?}"))) };
    match (local_only, remote_only) {
        (0, 0) => return Ok(PullOutcome::UpToDate),
        (n, 0) => return Ok(PullOutcome::Ahead { n }),
        (0, _) => {}
        _ => return Ok(PullOutcome::Diverged { local_only, remote_only }),
    }
    let head = git(repo, &["symbolic-ref", "--quiet", "HEAD"]).map_err(|error| refused(error.to_string()))?;
    let target_ref = format!("refs/heads/{target}");
    let on_target = head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == target_ref;
    if on_target {
        let files = changed_tracked_files(repo).map_err(refused)?;
        if !files.is_empty() {
            return Ok(PullOutcome::Dirty { files });
        }
        // An ignored file in the way (a local .env, say) stops the merge rather than being overwritten.
        let merged = git(repo, &["merge", "--ff-only", "--no-overwrite-ignore", "--quiet", to]).map_err(|error| refused(error.to_string()))?;
        if !merged.status.success() {
            return Err(refused(stderr(&merged)));
        }
    } else {
        if let Some(path) = checked_out_elsewhere(repo, &target_ref).map_err(refused)? {
            return Err(refused(format!("{target} is checked out in {path}; update it there")));
        }
        // Compare-and-swap on the old value: the ref moves only from the commit that was checked.
        let updated = git(repo, &["update-ref", &target_ref, to, &from]).map_err(|error| refused(error.to_string()))?;
        if !updated.status.success() {
            return Err(refused(stderr(&updated)));
        }
    }
    Ok(PullOutcome::FastForwarded { from, to: to.to_string() })
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// The worktree of `repo`'s repository, if any, that has `branch_ref` checked out.
fn checked_out_elsewhere(repo: &Path, branch_ref: &str) -> Result<Option<String>, String> {
    let listed = git(repo, &["worktree", "list", "--porcelain", "-z"]).map_err(|error| error.to_string())?;
    if !listed.status.success() {
        return Err(stderr(&listed));
    }
    let mut path = None;
    for field in listed.stdout.split(|b| *b == 0) {
        let field = String::from_utf8_lossy(field);
        if let Some(at) = field.strip_prefix("worktree ") {
            path = Some(at.to_string());
        } else if field.strip_prefix("branch ") == Some(branch_ref) {
            return Ok(path);
        }
    }
    Ok(None)
}

/// The tracked files the checkout at `repo` has changes to, staged or not, at most
/// [`DIRTY_FILES_NAMED`]; untracked files are not looked at.
fn changed_tracked_files(repo: &Path) -> Result<Vec<String>, String> {
    let status = git(repo, &["status", "--porcelain=v1", "-z", "--untracked-files=no"]).map_err(|error| error.to_string())?;
    if !status.status.success() {
        return Err(stderr(&status));
    }
    let mut files = Vec::new();
    let mut entries = status.stdout.split(|b| *b == 0).filter(|entry| entry.len() > 3);
    while let Some(entry) = entries.next() {
        files.push(String::from_utf8_lossy(&entry[3..]).into_owned());
        // A rename or copy is followed by the path it came from.
        if matches!(entry[0], b'R' | b'C') {
            entries.next();
        }
    }
    files.truncate(DIRTY_FILES_NAMED);
    Ok(files)
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

/// How many commits the local `branch` has that `remote/branch`, as last fetched, does not. A run branches
/// from the remote's target, so those commits are not in it. None when either branch is missing, a name is
/// unsafe, or git fails: there is then nothing to say.
pub fn local_commits_not_on_remote(repo: &Path, remote: &str, branch: &str) -> Option<usize> {
    check_remote(remote).ok()?;
    check_branch(repo, branch).ok()?;
    let range = format!("refs/remotes/{remote}/{branch}..refs/heads/{branch}");
    let counted = git(repo, &["rev-list", "--count", &range, "--"]).ok()?;
    if !counted.status.success() {
        return None;
    }
    String::from_utf8_lossy(&counted.stdout).trim().parse().ok()
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
