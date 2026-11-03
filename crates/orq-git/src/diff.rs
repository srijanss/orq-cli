//! Deterministic Git evidence for a review: the commits compared and what
//! changed between them. Read-only; only `rev-parse` and `diff` are run.

use std::path::Path;
use std::process::Command;

use crate::REPOSITORY_SELECTION_VARS;

/// One changed file. `added` and `deleted` are `None` for a binary file, whose
/// line counts Git does not report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// The previous path when the file was renamed.
    pub old_path: Option<String>,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEvidence {
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// Git could not run, or rejected a revision or the diff.
    Git(String),
    /// Git produced output this parser does not understand.
    Unparseable(String),
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, DiffError> {
    let mut command = Command::new("git");
    for var in REPOSITORY_SELECTION_VARS {
        command.env_remove(var);
    }
    let output = command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| DiffError::Git(err.to_string()))?;
    if !output.status.success() {
        return Err(DiffError::Git(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    Ok(output.stdout)
}

fn resolve(repo: &Path, rev: &str) -> Result<String, DiffError> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if sha.is_empty() {
        return Err(DiffError::Git(format!("cannot resolve {rev:?} to a commit")));
    }
    Ok(sha)
}

/// The changes from `base` to `head` in `repo`, with both revisions resolved
/// to full commit SHAs.
pub fn diff_evidence(repo: &Path, base: &str, head: &str) -> Result<DiffEvidence, DiffError> {
    let base_sha = resolve(repo, base)?;
    let head_sha = resolve(repo, head)?;
    let out = git(repo, &["diff", "--numstat", "-z", "-M", "--no-ext-diff", "--no-textconv", &base_sha, &head_sha, "--"])?;
    let files = parse_numstat(&out)?;
    Ok(DiffEvidence { base_sha, head_sha, files })
}

fn parse_numstat(out: &[u8]) -> Result<Vec<FileChange>, DiffError> {
    let bad = |what: &str| DiffError::Unparseable(what.to_string());
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut fields = out.split(|b| *b == 0).peekable();
    let mut files = Vec::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, |b| *b == b'\t');
        let (added, deleted, path) = (parts.next(), parts.next(), parts.next());
        let (Some(added), Some(deleted), Some(path)) = (added, deleted, path) else { return Err(bad("numstat record")) };
        let count = |bytes: &[u8]| -> Result<Option<u64>, DiffError> {
            if bytes == b"-" {
                return Ok(None);
            }
            std::str::from_utf8(bytes).ok().and_then(|s| s.parse().ok()).map(Some).ok_or_else(|| bad("line count"))
        };
        let (added, deleted) = (count(added)?, count(deleted)?);
        let (path, old_path) = if path.is_empty() {
            let old = fields.next().ok_or_else(|| bad("rename source"))?;
            let new = fields.next().ok_or_else(|| bad("rename target"))?;
            (text(new), Some(text(old)))
        } else {
            (text(path), None)
        };
        files.push(FileChange { path, old_path, added, deleted });
    }
    Ok(files)
}
