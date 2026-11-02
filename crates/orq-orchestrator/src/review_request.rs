//! The review-request stage: open the GitHub PR or GitLab MR for the run branch
//! against the stored target branch, read its URL and id back as structured
//! output, and record them. Only create and view are ever asked of the provider;
//! nothing here approves or merges.

use std::path::Path;
use std::time::Duration;

use crate::implement::{next_event_id, now_rfc3339};
use crate::review_draft::{run_drafter, DraftSession, ReviewDraft};
use crate::store::{Event, RunRecord, RunStore, StoreError};

const STAGE: &str = "review";
const CREATED: &str = "review.created";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRef {
    pub url: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewRequestOutcome {
    Created(ReviewRef),
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
    record_event(store, &record, "stage.started", serde_json::json!({ "provider": record.provider }))?;

    let session = |args: Vec<String>| DraftSession {
        program: program.to_string_lossy().into_owned(),
        args,
        env: provider_environment(),
        cwd: record.worktree.clone(),
    };
    if let Err(reason) = run_drafter(&session(create_args(kind, &record, draft)), timeout) {
        return fail(store, &record, format!("the review request could not be created: {reason}"));
    }
    let viewed = match run_drafter(&session(view_args(kind, &record)), timeout) {
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
    let found = store.events(run_id)?.into_iter().rev().find(|e| e.kind == CREATED);
    Ok(found.and_then(|e| {
        Some(ReviewRef { url: e.payload.get("url")?.as_str()?.to_string(), id: e.payload.get("id")?.as_str()?.to_string() })
    }))
}

fn create_args(kind: Kind, record: &RunRecord, draft: &ReviewDraft) -> Vec<String> {
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match kind {
        Kind::GitHub => strings(&[
            "pr", "create", "--head", &record.branch, "--base", &record.target_branch, "--title", &draft.title, "--body",
            &draft.body,
        ]),
        Kind::GitLab => strings(&[
            "mr",
            "create",
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

fn view_args(kind: Kind, record: &RunRecord) -> Vec<String> {
    let strings = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match kind {
        Kind::GitHub => strings(&["pr", "view", &record.branch, "--json", "number,url"]),
        Kind::GitLab => strings(&["mr", "view", &record.branch, "--output", "json"]),
    }
}

/// A web url and a numeric id, or nothing: the provider's text is untrusted.
fn parse_view(kind: Kind, text: &str) -> Option<ReviewRef> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
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
