//! The review-request stage: open the GitHub PR or GitLab MR for the run branch
//! against the stored target branch, read its URL and id back as structured
//! output, and record them. An open or merged review for the same branches is
//! adopted instead of duplicated. Only list, create and view are ever asked of
//! the provider; nothing here approves or merges.

use std::path::Path;
use std::time::Duration;

use crate::implement::{next_event_id, now_rfc3339};
use crate::outcome::git_raw;
use crate::review_draft::{run_drafter, DraftSession, ReviewDraft};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "review";
const CREATED: &str = "review.created";
const ADOPTED: &str = "review.adopted";
/// One page of the lookup. Reviews for a single pair of branches are few; a page that fills
/// up may be hiding older ones, so the lookup then fails rather than guess.
const LOOKUP_PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRef {
    pub url: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewRequestOutcome {
    Created(ReviewRef),
    /// An open or merged review for the same branches already existed and was recorded instead.
    Adopted(ReviewRef),
    /// The provider refused or answered with something unusable; the stage has failed.
    Failed { reason: String },
}

#[derive(Clone, Copy)]
enum Kind {
    GitHub,
    GitLab,
}

/// Open the review request for `run_id` with the provider CLI at `program`.
pub fn create_review_request(
    store: &RunStore,
    run_id: &str,
    program: &Path,
    draft: &ReviewDraft,
    timeout: Duration,
) -> Result<ReviewRequestOutcome, StoreError> {
    let record = store.load(run_id)?;
    let kind = match record.provider.as_str() {
        "github" => Kind::GitHub,
        "gitlab" => Kind::GitLab,
        other => return Err(StoreError::Corrupt(format!("the provider {other} cannot open review requests"))),
    };
    // The repository comes from the stored remote, never from the CLI's own inference.
    let repo = repository_url(&record)?;
    record_event(store, &record, "stage.started", serde_json::json!({ "provider": record.provider, "repository": repo }))?;

    let session = |args: Vec<String>| DraftSession {
        program: program.to_string_lossy().into_owned(),
        args,
        env: provider_environment(),
        cwd: record.worktree.clone(),
    };
    // A review for these branches may exist already (an earlier attempt whose read-back failed, or
    // someone else's); it is adopted, and when that cannot be told a blind create could duplicate it.
    match find_existing(kind, &session(list_args(kind, &record, &repo)), timeout) {
        Err(reason) => return fail(store, &record, format!("existing review requests could not be listed: {reason}")),
        Ok(Some(review)) => {
            record_event(
                store,
                &record,
                ADOPTED,
                serde_json::json!({ "url": review.url, "id": review.id, "provider": record.provider }),
            )?;
            record_event(store, &record, "stage.completed", serde_json::json!({ "url": review.url, "adopted": true }))?;
            return Ok(ReviewRequestOutcome::Adopted(review));
        }
        Ok(None) => {}
    }
    if let Err(reason) = run_drafter(&session(create_args(kind, &record, draft, &repo)), timeout) {
        return fail(store, &record, format!("the review request could not be created: {reason}"));
    }
    let viewed = match run_drafter(&session(view_args(kind, &record, &repo)), timeout) {
        Ok(text) => text,
        Err(reason) => return fail(store, &record, format!("the new review request could not be read back: {reason}")),
    };
    let Some(review) = parse_view(kind, &viewed) else {
        return fail(store, &record, "the provider's answer has no usable url and id".to_string());
    };

    record_event(
        store,
        &record,
        CREATED,
        serde_json::json!({ "url": review.url, "id": review.id, "provider": record.provider }),
    )?;
    record_event(store, &record, "stage.completed", serde_json::json!({ "url": review.url }))?;
    Ok(ReviewRequestOutcome::Created(review))
}

/// The session starts from an empty environment; these are the variables the
/// provider CLIs read for credentials, host and configuration, and only these pass.
const FORWARDED_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_CONFIG_DIR",
    "GITLAB_TOKEN",
    "GITLAB_ACCESS_TOKEN",
    "OAUTH_TOKEN",
    "GITLAB_HOST",
    "GL_HOST",
    "GLAB_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
];

fn provider_environment() -> Vec<(String, String)> {
    FORWARDED_ENV.iter().filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value))).collect()
}

/// The review request recorded for `run_id`, if one was created.
pub fn recorded_review(store: &RunStore, run_id: &str) -> Result<Option<ReviewRef>, StoreError> {
    let found = store.events(run_id)?.into_iter().rev().find(|e| e.kind == CREATED || e.kind == ADOPTED);
    Ok(found.and_then(|e| {
        Some(ReviewRef { url: e.payload.get("url")?.as_str()?.to_string(), id: e.payload.get("id")?.as_str()?.to_string() })
    }))
}

/// The stored remote as a credential-free https URL the provider CLIs accept.
fn repository_url(record: &RunRecord) -> Result<String, StoreError> {
    let unresolved = || StoreError::Corrupt(format!("the remote {} has no repository url", record.remote));
    // Where the branch is pushed (pushurl and pushInsteadOf apply), which is where the review belongs.
    let got = git_raw(&record.worktree, &["remote", "get-url", "--push", "--all", "--", &record.remote])?;
    let mut urls = got.stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    match (got.success, urls.next(), urls.next()) {
        (true, Some(url), None) => normalize_remote(url).ok_or_else(unresolved),
        _ => Err(unresolved()),
    }
}

fn normalize_remote(raw: &str) -> Option<String> {
    // A query or fragment may carry a credential, and is never part of a repository's identity.
    if raw.is_empty() || raw.chars().any(|c| c.is_whitespace() || c.is_control() || c == '?' || c == '#') {
        return None;
    }
    let (scheme, authority_and_path) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest.to_string()),
        // scp-like: [user@]host:path
        None => {
            let (host, path) = raw.split_once(':')?;
            if host.contains('/') {
                return None;
            }
            (None, format!("{host}/{}", path.trim_start_matches('/')))
        }
    };
    let (authority, path) = authority_and_path.split_once('/')?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    // A port on http(s) is the web port; on ssh or git it is not, and is dropped.
    let web = matches!(scheme, Some("http" | "https"));
    let host = if web { host_port } else { host_port.split(':').next().unwrap_or("") };
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let scheme = if scheme == Some("http") { "http" } else { "https" };
    Some(format!("{scheme}://{host}/{path}"))
}

fn create_args(kind: Kind, record: &RunRecord, draft: &ReviewDraft, repo: &str) -> Vec<String> {
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match kind {
        Kind::GitHub => strings(&[
            "pr", "create", "--repo", repo, "--head", &record.branch, "--base", &record.target_branch, "--title",
            &draft.title, "--body", &draft.body,
        ]),
        Kind::GitLab => strings(&[
            "mr",
            "create",
            "--repo",
            repo,
            "--source-branch",
            &record.branch,
            "--target-branch",
            &record.target_branch,
            "--title",
            &draft.title,
            "--description",
            &draft.body,
            "--yes",
        ]),
    }
}

fn list_args(kind: Kind, record: &RunRecord, repo: &str) -> Vec<String> {
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let page = LOOKUP_PAGE.to_string();
    match kind {
        Kind::GitHub => strings(&[
            "pr", "list", "--repo", repo, "--head", &record.branch, "--base", &record.target_branch, "--state", "all",
            "--json", "number,url,state,isCrossRepository", "--limit", &page,
        ]),
        Kind::GitLab => strings(&[
            "mr",
            "list",
            "--repo",
            repo,
            "--source-branch",
            &record.branch,
            "--target-branch",
            &record.target_branch,
            "--all",
            "--per-page",
            &page,
            "--output",
            "json",
        ]),
    }
}

/// The open review for these branches, else a merged one; closed ones are not reused.
fn find_existing(kind: Kind, session: &DraftSession, timeout: Duration) -> Result<Option<ReviewRef>, String> {
    let text = run_drafter(session, timeout)?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(text.trim()).map_err(|_| "the provider's listing is not a json array".to_string())?;
    if rows.len() >= LOOKUP_PAGE {
        return Err(format!("the listing filled its page of {LOOKUP_PAGE}, so it may be incomplete"));
    }
    let (mut open, mut merged) = (None, None);
    for row in &rows {
        // Only a state known to be reusable or known not to be is accepted; anything else
        // means the lookup cannot be trusted, and a blind create could duplicate.
        let state = row
            .get("state")
            .and_then(|s| s.as_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| "a listed review has no state".to_string())?;
        // A review from a fork can share the branch name; it does not review this run's branch.
        if !from_own_repository(kind, row)? {
            continue;
        }
        let slot = match state.as_str() {
            "open" | "opened" => &mut open,
            "merged" => &mut merged,
            "closed" | "locked" => continue,
            other => return Err(format!("a listed review has the unknown state {other}")),
        };
        let review = review_from(kind, row).ok_or_else(|| "a listed review has no usable url and id".to_string())?;
        slot.get_or_insert(review);
    }
    Ok(open.or(merged))
}

/// Whether the review's source lives in the repository the run was pushed to; an answer that
/// does not say is an error, never a guess.
fn from_own_repository(kind: Kind, row: &serde_json::Value) -> Result<bool, String> {
    let unsaid = || "a listed review does not say where its source branch lives".to_string();
    match kind {
        Kind::GitHub => Ok(!row.get("isCrossRepository").and_then(|v| v.as_bool()).ok_or_else(unsaid)?),
        Kind::GitLab => {
            let source = row.get("source_project_id").and_then(|v| v.as_u64()).ok_or_else(unsaid)?;
            let target = row.get("target_project_id").and_then(|v| v.as_u64()).ok_or_else(unsaid)?;
            Ok(source == target)
        }
    }
}

fn view_args(kind: Kind, record: &RunRecord, repo: &str) -> Vec<String> {
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match kind {
        Kind::GitHub => strings(&["pr", "view", &record.branch, "--repo", repo, "--json", "number,url"]),
        Kind::GitLab => strings(&["mr", "view", &record.branch, "--repo", repo, "--output", "json"]),
    }
}

/// A web url and a numeric id, or nothing: the provider's text is untrusted.
fn parse_view(kind: Kind, text: &str) -> Option<ReviewRef> {
    review_from(kind, &serde_json::from_str(text.trim()).ok()?)
}

fn review_from(kind: Kind, value: &serde_json::Value) -> Option<ReviewRef> {
    let (id_key, url_key) = match kind {
        Kind::GitHub => ("number", "url"),
        Kind::GitLab => ("iid", "web_url"),
    };
    let id = value.get(id_key)?.as_u64()?.to_string();
    let url = value.get(url_key)?.as_str()?;
    let web = url.starts_with("https://") || url.starts_with("http://");
    if !web || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(ReviewRef { url: url.to_string(), id })
}

fn fail(store: &RunStore, record: &RunRecord, reason: String) -> Result<ReviewRequestOutcome, StoreError> {
    record_event(store, record, "stage.failed", serde_json::json!({ "reason": reason }))?;
    Ok(ReviewRequestOutcome::Failed { reason })
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
