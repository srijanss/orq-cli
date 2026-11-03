//! Looking at a review on the provider, read-only. This builds the one
//! `view` command Orq may run, and turns what `gh`/`glab` print (or fail
//! with) into an observation for the review lifecycle. It never builds a
//! command that changes the review.

use serde_json::Value;

use crate::review_lifecycle::{Observation, ObservationError, ProviderState};
use crate::storage::check_head_sha;

const MAX_DETAIL: usize = 500;

/// The read-only command that shows review `review_id` of `repository`.
pub fn view_command(provider: &str, repository: &str, review_id: &str) -> Result<(String, Vec<String>), String> {
    if review_id.is_empty() || !review_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("review id {review_id:?} is not a number"));
    }
    let repository_ok = !repository.is_empty()
        && !repository.starts_with('-')
        && repository.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if !repository_ok {
        return Err(format!("repository {repository:?} is not a plain owner/name path"));
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
    let state = match (provider, state_name) {
        ("github", "OPEN") | ("gitlab", "opened") => ProviderState::Open,
        ("github", "MERGED") | ("gitlab", "merged") => ProviderState::Merged,
        ("github", "CLOSED") | ("gitlab", "closed") => ProviderState::Closed,
        _ => return Err(unreadable("unknown state")),
    };
    let head = match provider {
        "github" => value.get("headRefOid"),
        _ => value.get("sha").filter(|v| v.is_string()).or_else(|| value.get("diff_refs").and_then(|r| r.get("head_sha"))),
    };
    let head_sha = head.and_then(Value::as_str).ok_or_else(|| unreadable("no head commit"))?;
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
