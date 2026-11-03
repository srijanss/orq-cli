//! Looking at a review on the provider, read-only. This builds the one
//! `view` command Orq may run, and turns what `gh`/`glab` print (or fail
//! with) into an observation for the review lifecycle. It never builds a
//! command that changes the review.

use serde_json::Value;

use crate::review_lifecycle::{Candidate, Observation, ObservationError, ProviderState};
use crate::storage::check_head_sha;

const MAX_DETAIL: usize = 500;

/// How many reviews one listing asks for. A listing this full may be missing
/// some, so it is never trusted.
pub const LIST_PAGE: usize = 100;

/// A review found by a listing, with the head commit it reviews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub candidate: Candidate,
    pub head_sha: String,
}

fn plain_path(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn repository_ok(repository: &str) -> bool {
    match repository.strip_prefix("https://") {
        Some(rest) => !rest.is_empty() && !rest.starts_with('/') && rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':')),
        None => plain_path(repository),
    }
}

fn map_state(provider: &str, name: &str) -> Option<ProviderState> {
    match (provider, name.to_lowercase().as_str()) {
        ("github", "open") | ("gitlab", "opened") => Some(ProviderState::Open),
        (_, "merged") => Some(ProviderState::Merged),
        ("github", "closed") | ("gitlab", "closed") | ("gitlab", "locked") => Some(ProviderState::Closed),
        _ => None,
    }
}

fn head_of<'a>(provider: &str, value: &'a Value) -> Option<&'a str> {
    let head = match provider {
        "github" => value.get("headRefOid"),
        _ => value.get("sha").filter(|v| v.is_string()).or_else(|| value.get("diff_refs").and_then(|r| r.get("head_sha"))),
    };
    head.and_then(Value::as_str)
}

/// The read-only command that lists every review (any state) from `branch`
/// into `target`.
pub fn list_command(provider: &str, repository: &str, branch: &str, target: &str) -> Result<(String, Vec<String>), String> {
    if !repository_ok(repository) {
        return Err(format!("repository {repository:?} is not a plain owner/name path or https address"));
    }
    if !plain_path(branch) || !plain_path(target) {
        return Err(format!("branch names {branch:?} / {target:?} are not plain names"));
    }
    let page = LIST_PAGE.to_string();
    let (program, args): (&str, Vec<&str>) = match provider {
        "github" => ("gh", vec!["pr", "list", "--repo", repository, "--head", branch, "--base", target, "--state", "all", "--json", "number,url,state,isCrossRepository,headRefOid", "--limit", &page]),
        "gitlab" => ("glab", vec!["mr", "list", "--repo", repository, "--source-branch", branch, "--target-branch", target, "--all", "--output", "json", "--per-page", &page]),
        other => return Err(format!("unknown provider {other:?}")),
    };
    let args: Vec<String> = args.into_iter().map(String::from).collect();
    if let Some(reason) = crate::no_merge::forbidden_provider_call(program, &args) {
        return Err(format!("refusing to run a forbidden provider call: {reason}"));
    }
    Ok((program.to_string(), args))
}

/// The reviews in a provider's listing. Reviews from forks are skipped; an
/// answer that is incomplete or not understood is `Unavailable`.
pub fn parse_list(provider: &str, output: &str) -> Result<Vec<Listed>, ObservationError> {
    let unreadable = |what: &str| ObservationError::Unavailable(format!("the provider's answer was not understood: {what}"));
    let value: Value = serde_json::from_str(output).map_err(|_| unreadable("not JSON"))?;
    let rows = value.as_array().ok_or_else(|| unreadable("not a list"))?;
    if rows.len() >= LIST_PAGE {
        return Err(unreadable("the list is full and may be incomplete"));
    }
    let mut listed = Vec::new();
    for row in rows {
        let fork = match provider {
            "github" => row.get("isCrossRepository").and_then(Value::as_bool).ok_or_else(|| unreadable("no fork flag"))?,
            _ => {
                let source = row.get("source_project_id").and_then(Value::as_u64).ok_or_else(|| unreadable("no source project"))?;
                let target = row.get("target_project_id").and_then(Value::as_u64).ok_or_else(|| unreadable("no target project"))?;
                source != target
            }
        };
        let (id_key, url_key) = if provider == "github" { ("number", "url") } else { ("iid", "web_url") };
        let id = row.get(id_key).and_then(Value::as_u64).ok_or_else(|| unreadable("no review number"))?;
        let url = row.get(url_key).and_then(Value::as_str).filter(|u| u.starts_with("https://") || u.starts_with("http://")).ok_or_else(|| unreadable("no review address"))?;
        let state = row.get("state").and_then(Value::as_str).and_then(|n| map_state(provider, n)).ok_or_else(|| unreadable("unknown state"))?;
        let head = head_of(provider, row).ok_or_else(|| unreadable("no head commit"))?;
        check_head_sha(head).map_err(|_| unreadable("head commit is not a commit id"))?;
        if fork {
            continue;
        }
        listed.push(Listed { candidate: Candidate { review_id: id.to_string(), url: url.to_string(), state }, head_sha: head.to_string() });
    }
    Ok(listed)
}

/// The read-only command that shows review `review_id` of `repository`.
pub fn view_command(provider: &str, repository: &str, review_id: &str) -> Result<(String, Vec<String>), String> {
    if review_id.is_empty() || !review_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("review id {review_id:?} is not a number"));
    }
    if !repository_ok(repository) {
        return Err(format!("repository {repository:?} is not a plain owner/name path or https address"));
    }
    let (program, args): (&str, Vec<&str>) = match provider {
        "github" => ("gh", vec!["pr", "view", review_id, "--repo", repository, "--json", "state,headRefOid"]),
        "gitlab" => ("glab", vec!["mr", "view", review_id, "--repo", repository, "--output", "json"]),
        other => return Err(format!("unknown provider {other:?}")),
    };
    let args: Vec<String> = args.into_iter().map(String::from).collect();
    if let Some(reason) = crate::no_merge::forbidden_provider_call(program, &args) {
        return Err(format!("refusing to run a forbidden provider call: {reason}"));
    }
    Ok((program.to_string(), args))
}

/// The state and head commit in a provider's `view` output.
pub fn parse_view(provider: &str, output: &str) -> Result<Observation, ObservationError> {
    let unreadable = |what: &str| ObservationError::Unavailable(format!("the provider's answer was not understood: {what}"));
    let value: Value = serde_json::from_str(output).map_err(|_| unreadable("not JSON"))?;
    let state_name = value.get("state").and_then(Value::as_str).ok_or_else(|| unreadable("no state"))?;
    let state = map_state(provider, state_name).ok_or_else(|| unreadable("unknown state"))?;
    let head_sha = head_of(provider, &value).ok_or_else(|| unreadable("no head commit"))?;
    check_head_sha(head_sha).map_err(|_| unreadable("head commit is not a commit id"))?;
    Ok(Observation { state, head_sha: head_sha.to_string() })
}

/// What a failed `view` call means: a refusal to read is told apart from an
/// outage. The detail is redacted and bounded before it is kept anywhere.
pub fn classify_failure(stderr: &str) -> ObservationError {
    let lowered = stderr.to_lowercase();
    let denied = ["forbidden", "unauthorized", "bad credentials", "auth login", "not accessible", "permission denied", "http 401", "http 403"];
    let redacted = crate::redact::redact(stderr.trim());
    let mut detail: String = redacted.chars().take(MAX_DETAIL).collect();
    if detail.is_empty() {
        detail = "no details were given".to_string();
    }
    if denied.iter().any(|needle| lowered.contains(needle)) {
        ObservationError::PermissionDenied(detail)
    } else {
        ObservationError::Unavailable(detail)
    }
}
