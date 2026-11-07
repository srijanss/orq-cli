//! Whether a run's implement session stopped because its review loop escalated, read off the
//! review ledger the session left in its worktree. Such a run waits for a human decision; it has
//! not failed.

use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::outcome::git_raw;

const ROUNDS_FILE: &str = ".tdd-review-rounds.json";
const FINDINGS_FILE: &str = ".tdd-review-findings.json";
/// A ledger is a few kilobytes; anything far larger is not one and is not read whole.
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
/// The review loop escalates when a blocking finding remains after this many rounds.
const ROUND_CAP: u64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalatedReview {
    /// The commit the escalated review started from.
    pub start: String,
    /// The head the last round reviewed.
    pub reviewed_head: String,
    /// Rounds run in that review.
    pub rounds: u64,
    /// The findings that blocked the review when it escalated, with their status now.
    pub findings: Vec<EscalatedFinding>,
    /// Commits made after the last reviewed head, oldest first: no reviewer has seen them.
    pub unreviewed_commits: Vec<UnreviewedCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalatedFinding {
    pub id: String,
    pub severity: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub claim: String,
    /// `open`, `rejected` or `deferred` as the ledger has it now, or `fixed` once it left it.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreviewedCommit {
    pub sha: String,
    pub subject: String,
}

/// The review in `worktree` that escalated, if its latest round did. Only reviews of this run's
/// work count: one starting at `base` or after it, whose reviewed head is still in the branch.
/// No ledger, or a latest round that did not escalate, is `None`.
pub fn escalated_review(worktree: &Path, base: &str) -> Result<Option<EscalatedReview>, String> {
    let Some(rounds) = read_ledger(&worktree.join(ROUNDS_FILE))? else {
        return Ok(None);
    };
    let Some(scopes) = rounds.as_object() else {
        return Err(format!("{ROUNDS_FILE} is not a JSON object"));
    };
    // The latest round of all the reviews this run's work could have had.
    let mut latest: Option<(&str, &Value)> = None;
    for list in scopes.values() {
        let Some(list) = list.as_array() else { continue };
        for round in list {
            let at = text(round, "recordedAt").unwrap_or("");
            if latest.is_none_or(|(seen, _)| at > seen) {
                latest = Some((at, round));
            }
        }
    }
    let Some((_, round)) = latest else { return Ok(None) };
    let (Some(start), Some(head)) = (text(round, "start"), text(round, "head")) else {
        return Ok(None);
    };
    if !is_ancestor(worktree, base, start)? || !is_ancestor(worktree, head, "HEAD")? {
        return Ok(None);
    }
    let number = round.get("round").and_then(Value::as_u64).unwrap_or(0);
    let blocking: Vec<&Value> = round
        .get("findings")
        .and_then(Value::as_array)
        .map(|f| f.iter().filter(|f| is_blocking(f)).collect())
        .unwrap_or_default();
    let rejected_twice = blocking
        .iter()
        .any(|f| f.get("rejectedCount").and_then(Value::as_u64).unwrap_or(0) >= 2);
    if blocking.is_empty() || (number < ROUND_CAP && !rejected_twice) {
        return Ok(None);
    }
    let live = read_ledger(&worktree.join(FINDINGS_FILE))?;
    let live = live
        .as_ref()
        .and_then(|l| l.get(format!("review:{start}")))
        .and_then(Value::as_array);
    let findings = blocking
        .iter()
        .map(|f| {
            let id = text(f, "id").unwrap_or("").to_string();
            let status = live
                .and_then(|l| l.iter().find(|n| text(n, "id") == Some(id.as_str())))
                .and_then(|n| text(n, "status"))
                .unwrap_or("fixed")
                .to_string();
            EscalatedFinding {
                severity: text(f, "severity").unwrap_or("").to_string(),
                file: text(f, "file").map(str::to_string),
                line: f.get("line").and_then(Value::as_u64),
                claim: text(f, "claim").unwrap_or("").to_string(),
                id,
                status,
            }
        })
        .collect();
    Ok(Some(EscalatedReview {
        start: start.to_string(),
        reviewed_head: head.to_string(),
        rounds: number,
        findings,
        unreviewed_commits: commits_after(worktree, head)?,
    }))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Open and at medium severity or above: what keeps a review loop from ending.
fn is_blocking(finding: &Value) -> bool {
    text(finding, "status") == Some("open")
        && matches!(text(finding, "severity"), Some("medium" | "high" | "critical"))
}

/// Read a ledger file, `None` when there is none. Opened without following a link and without
/// waiting on a pipe, then judged by the handle, since the path may be swapped after a check.
fn read_ledger(path: &Path) -> Result<Option<Value>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot open {name}: {e}")),
    };
    let meta = file.metadata().map_err(|e| format!("cannot read {name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {name}: {e}"))?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err(format!("{name} is larger than {MAX_LEDGER_BYTES} bytes"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| format!("{name} is not valid JSON: {e}"))
}

/// Whether `ancestor` is `descendant` or one of its ancestors. A name git does not know is not.
fn is_ancestor(worktree: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    if !is_commit_name(ancestor) || !(descendant == "HEAD" || is_commit_name(descendant)) {
        return Ok(false);
    }
    let out = git_raw(worktree, &["merge-base", "--is-ancestor", ancestor, descendant])
        .map_err(|e| e.to_string())?;
    Ok(out.success)
}

/// A hexadecimal object name, so a ledger value can never be read by git as an option.
fn is_commit_name(name: &str) -> bool {
    (4..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn commits_after(worktree: &Path, head: &str) -> Result<Vec<UnreviewedCommit>, String> {
    let range = format!("{head}..HEAD");
    let out = git_raw(worktree, &["log", "--reverse", "--format=%H%x09%s", &range, "--"])
        .map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!("git log {range} failed: {}", out.stderr.trim()));
    }
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, subject)| UnreviewedCommit { sha: sha.to_string(), subject: subject.to_string() })
        .collect())
}
